//! AST-rewriting pre-resolve passes that eliminate sugar so downstream
//! phases only see the canonical form.
//!
//! Today this houses one pass: slice 2 of the `impl Trait` epic —
//! argument-position `impl Trait` desugars to a fresh anonymous generic
//! parameter on the enclosing function. See `docs/design.md § `impl
//! Trait` (Existential Types)` and `phase-5-diagnostics.md` line 395.
//!
//! Pipeline placement: between [`crate::parse`] and [`crate::resolve`].
//! The compilation drivers in `lib.rs` and `cli.rs` invoke
//! [`desugar_program`] on the mutable `Program` before resolution; the
//! formatter path deliberately skips this pass so `impl Trait` round-trips
//! verbatim.

use crate::ast::*;
use crate::token::Span;

/// Run every AST-rewriting pre-resolve pass over `program` in place.
/// Today: argument-position `impl Trait` desugar (slice 2).
pub fn desugar_program(program: &mut Program) {
    desugar_impl_trait_args_in_program(program);
}

// ── `impl Trait` argument-position desugar ──────────────────────

fn desugar_impl_trait_args_in_program(program: &mut Program) {
    for item in &mut program.items {
        match item {
            Item::Function(f) => desugar_impl_trait_args_in_function(f),
            Item::ImplBlock(imp) => {
                for it in &mut imp.items {
                    if let ImplItem::Method(method) = it {
                        desugar_impl_trait_args_in_function(method);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Rewrite every top-level `TypeKind::ImplTrait` on `f.params[i].ty` into a
/// `TypeKind::Path` reference to a freshly synthesized anonymous generic
/// parameter `T_impl_arg_N`, and append that parameter (with the original
/// trait as its bound) to `f.generic_params`. Per-occurrence: two
/// `impl T` parameters produce two distinct synthetic params so the
/// typechecker never unifies them.
///
/// Only top-level argument-position occurrences are desugared. Return-position
/// `impl Trait` (slice 3) and TAIT-RHS `impl Trait` (slice 6) are intentionally
/// left intact so the typechecker's slice-1 stub continues to surface them.
/// Nested-through-generic-args (`Vec[impl T]`) and trait-method argument
/// position were already rejected at parse (slice 1), so they never reach
/// this pass.
///
/// `use_effects` on argument-position `impl Trait` is dropped: per the parent
/// spec the argument-position desugar produces "the same bounds (no
/// existential, no special handling downstream)" — the `with E'` clause is
/// meaningful only on return-position existentials, where slice 3 + Phase 8
/// pick it up.
fn desugar_impl_trait_args_in_function(f: &mut Function) {
    let mut synthetic_params: Vec<GenericParam> = Vec::new();
    let mut counter = 0usize;
    for param in &mut f.params {
        let TypeKind::ImplTrait {
            trait_path,
            args,
            span: impl_trait_span,
            ..
        } = &param.ty.kind
        else {
            continue;
        };

        let synthetic_name = format!("T_impl_arg_{counter}");
        counter += 1;

        let bound = TraitBound {
            path: trait_path.segments.clone(),
            generic_args: if args.is_empty() {
                None
            } else {
                Some(args.clone())
            },
            span: impl_trait_span.clone(),
        };
        synthetic_params.push(GenericParam {
            name: synthetic_name.clone(),
            bounds: vec![bound],
            is_const: false,
            const_type: None,
            variance: Variance::Invariant,
            variance_span: None,
            is_variadic_shape: false,
            span: impl_trait_span.clone(),
        });

        let original_span = param.ty.span.clone();
        param.ty = TypeExpr {
            kind: TypeKind::Path(PathExpr {
                segments: vec![synthetic_name],
                generic_args: None,
                span: original_span.clone(),
            }),
            span: original_span,
        };
    }

    if synthetic_params.is_empty() {
        return;
    }

    match &mut f.generic_params {
        Some(existing) => existing.params.extend(synthetic_params),
        None => {
            let span = synthetic_params
                .first()
                .map(|p| p.span.clone())
                .unwrap_or_else(|| Span {
                    line: 0,
                    column: 0,
                    offset: 0,
                    length: 0,
                });
            f.generic_params = Some(GenericParams {
                params: synthetic_params,
                effect_params: Vec::new(),
                span,
            });
        }
    }
}
