//! Runtime predicate emission for refinement types (phase-9 step 5c).
//!
//! Step 4 lowered a refinement to its base *layout*; step 5a made codegen
//! *dispatch* a refined value as its base. This module closes the
//! value-safety gap: the predicate a refinement carries
//! (`type Even = i64 where self % 2 == 0`) is actually *enforced* at the
//! two construction sites.
//!
//! - `x as Refined` (asserting form) → `emit_refinement_assert`: a false
//!   predicate aborts with a `contract violated` fault.
//! - `Refined.try_from(x)` (recoverable form) → `compile_refinement_try_from`:
//!   `Ok(x)` when the predicate holds, `Err(<message>)` when it fails.
//!
//! Both bind the candidate value to a synthetic `self` local and compile
//! the predicate against it, reusing the normal expression-compilation and
//! method-dispatch machinery (so method-form predicates such as
//! `self.len() > 0` work). The predicate's `self` references are rewritten
//! to that local via `subst_self`, avoiding any clobber of a real method
//! receiver at the cast site.

use crate::ast::{CallArg, Expr, ExprKind, FieldInit};
use crate::token::Span;

use inkwell::values::{BasicValueEnum, IntValue};

use super::state::VarSlot;

/// Synthetic local the candidate value is bound to while the predicate is
/// evaluated. The `__karac_`-prefix keeps it clear of any user identifier.
const REFINE_SELF: &str = "__karac_refine_self";

/// Rewrite every `self` reference in a predicate expression to reference
/// `name` instead. The typechecker restricts refinement predicates to
/// `self`-rooted forms over constants and a fixed operator set
/// (`validate_refinement_predicate`), so this targeted walk covers every
/// shape a valid predicate can take; any other form is left untouched (it
/// would have been rejected upstream).
fn subst_self(e: &mut Expr, name: &str) {
    match &mut e.kind {
        ExprKind::SelfValue => {
            e.kind = ExprKind::Identifier(name.to_string());
        }
        ExprKind::Binary { left, right, .. } => {
            subst_self(left, name);
            subst_self(right, name);
        }
        ExprKind::Unary { operand, .. } => subst_self(operand, name),
        ExprKind::FieldAccess { object, .. } => subst_self(object, name),
        ExprKind::MethodCall { object, args, .. } => {
            subst_self(object, name);
            for a in args.iter_mut() {
                subst_self(&mut a.value, name);
            }
        }
        ExprKind::Call { callee, args } => {
            subst_self(callee, name);
            for a in args.iter_mut() {
                subst_self(&mut a.value, name);
            }
        }
        ExprKind::Question(inner) => subst_self(inner, name),
        _ => {}
    }
}

impl<'ctx> super::Codegen<'ctx> {
    /// Bind `value` to the synthetic `self` local, registering the base
    /// type's side-tables so method-form predicates dispatch correctly.
    fn bind_refine_self(&mut self, value: BasicValueEnum<'ctx>, rname: &str) {
        let fn_val = self.current_fn.expect("refinement check inside a function");
        let alloca = self.create_entry_alloca(fn_val, REFINE_SELF, value.get_type());
        self.builder.build_store(alloca, value).unwrap();
        self.variables.insert(
            REFINE_SELF.to_string(),
            VarSlot {
                ptr: alloca,
                ty: value.get_type(),
            },
        );
        // The base `TypeExpr` lives in `refinement_bases` for a plain
        // refinement and in `distinct_bases` for a combined `distinct type T
        // = Base where pred`; consult both so a method-form predicate
        // (`self.len()`) gets the base side-tables in either case.
        if let Some(base_te) = self
            .contract_state
            .refinement_bases
            .get(rname)
            .or_else(|| self.contract_state.distinct_bases.get(rname))
            .cloned()
        {
            self.register_var_from_type_expr(REFINE_SELF, &base_te);
        }
    }

    /// Drop the synthetic `self` binding's side-table entries so a stale
    /// registration can't leak into later code in the same function.
    fn unbind_refine_self(&mut self) {
        self.variables.remove(REFINE_SELF);
        self.var_types.var_type_names.remove(REFINE_SELF);
        self.var_types.string_vars.remove(REFINE_SELF);
        self.var_types.vec_elem_types.remove(REFINE_SELF);
        self.var_types.var_elem_type_exprs.remove(REFINE_SELF);
    }

    /// Compile the refinement's predicate to an `i1`, with `self` already
    /// bound (caller must `bind_refine_self` first and `unbind_refine_self`
    /// after the binding is no longer needed).
    fn compile_bound_predicate(&mut self, rname: &str) -> Result<IntValue<'ctx>, String> {
        let mut pred = self
            .contract_state
            .refinement_predicates
            .get(rname)
            .cloned()
            .ok_or_else(|| format!("no predicate registered for refinement `{rname}`"))?;
        subst_self(&mut pred, REFINE_SELF);
        Ok(self.compile_expr(&pred)?.into_int_value())
    }

    /// `x as Refined`: enforce the predicate at runtime, aborting with a
    /// `contract violated` fault when it fails. A no-op when `rname` is not
    /// a refinement. On success the builder is left in a block where the
    /// predicate held; the caller's (layout-identical) value stays valid.
    pub(super) fn emit_refinement_assert(
        &mut self,
        rname: &str,
        value: BasicValueEnum<'ctx>,
    ) -> Result<(), String> {
        if !self
            .contract_state
            .refinement_predicates
            .contains_key(rname)
        {
            return Ok(());
        }
        self.bind_refine_self(value, rname);
        let cond = self.compile_bound_predicate(rname)?;
        self.unbind_refine_self();

        let fn_val = self
            .current_fn
            .ok_or_else(|| "refinement check emitted outside a function".to_string())?;
        let fail_bb = self.context.append_basic_block(fn_val, "refine.fail");
        let ok_bb = self.context.append_basic_block(fn_val, "refine.ok");
        self.builder
            .build_conditional_branch(cond, ok_bb, fail_bb)
            .unwrap();
        self.builder.position_at_end(fail_bb);
        self.emit_panic(&format!(
            "contract violated: value does not satisfy refinement `{rname}`"
        ));
        self.builder.build_unreachable().unwrap();
        self.builder.position_at_end(ok_bb);
        Ok(())
    }

    /// `Refined.try_from(x)`: lower to a runtime predicate check producing a
    /// `Result[Refined, String]` — `Ok(x)` when the predicate holds,
    /// `Err(<message>)` otherwise. Returns `Ok(None)` when `rname` is not a
    /// refinement so the caller falls through to normal dispatch.
    /// `<C-like #[repr(intN)] enum>.try_from(v)` — design.md § Enum
    /// Discriminant Runtime Surface (B-2026-08-21-26). Returns `Ok(None)` for
    /// any head that is not a C-like enum in the discriminant table, so a
    /// refinement / distinct / primitive `try_from` falls through untouched.
    ///
    /// Shape: one comparison per declared variant, each with its own `Ok`
    /// block, and a single trailing `Err` block, joined by a phi. Both arms
    /// are built through the ORDINARY variant constructors
    /// (`try_compile_enum_variant` for `Ok`, `compile_enum_struct_variant_init`
    /// for `OutOfRange { value }`) rather than by stamping tags by hand, so
    /// the payload layouts stay whatever the rest of codegen decided — the
    /// `.discriminant()` twin reads those same layouts in the other direction.
    ///
    /// The argument is compiled ONCE into a synthetic local and referenced by
    /// name afterwards: it may carry side effects, and it is read again on the
    /// `Err` path to fill `value`.
    pub(super) fn compile_enum_try_from(
        &mut self,
        enum_name: &str,
        arg: &Expr,
        span: &Span,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        let Some(disc) = self.type_decls.enum_discriminants.get(enum_name).cloned() else {
            return Ok(None);
        };
        let fn_val = self
            .current_fn
            .ok_or_else(|| "enum try_from emitted outside a function".to_string())?;

        // Compile the argument once, into a named slot the synthesized
        // `Identifier` expressions below resolve against.
        let raw = self.compile_expr(arg)?;
        let raw_int = match raw {
            BasicValueEnum::IntValue(iv) => iv,
            other => {
                return Err(format!(
                    "enum try_from: '{enum_name}' argument has non-integer representation {other:?}"
                ))
            }
        };
        let slot_name = format!("__karac_tryfrom_{enum_name}");
        let alloca = self.create_entry_alloca(fn_val, &slot_name, raw.get_type());
        self.builder.build_store(alloca, raw).unwrap();
        self.variables.insert(
            slot_name.clone(),
            VarSlot {
                ptr: alloca,
                ty: raw.get_type(),
            },
        );
        let raw_ident = Expr {
            kind: ExprKind::Identifier(slot_name.clone()),
            span: *span,
        };

        let cont_bb = self.context.append_basic_block(fn_val, "enumtryfrom.cont");
        let mut incoming: Vec<(BasicValueEnum<'ctx>, inkwell::basic_block::BasicBlock<'ctx>)> =
            Vec::with_capacity(disc.values.len() + 1);

        for (variant, declared) in &disc.values {
            let hit_bb = self.context.append_basic_block(fn_val, "enumtryfrom.hit");
            let next_bb = self.context.append_basic_block(fn_val, "enumtryfrom.next");
            let want = raw_int.get_type().const_int(*declared as u64, true);
            let eq = self
                .builder
                .build_int_compare(inkwell::IntPredicate::EQ, raw_int, want, "enumtryfrom.eq")
                .unwrap();
            self.builder
                .build_conditional_branch(eq, hit_bb, next_bb)
                .unwrap();

            self.builder.position_at_end(hit_bb);
            let variant_expr = Expr {
                kind: ExprKind::Path {
                    segments: vec![enum_name.to_string(), variant.clone()],
                    generic_args: None,
                },
                span: *span,
            };
            let ok_arg = CallArg {
                label: None,
                mut_marker: false,
                mut_marker_span: None,
                value: variant_expr,
                span: *span,
            };
            let ok_val = self
                .try_compile_enum_variant("Ok", Some("Result"), std::slice::from_ref(&ok_arg))?
                .ok_or_else(|| {
                    format!("enum try_from: failed to build Ok({enum_name}.{variant})")
                })?;
            incoming.push((ok_val, self.builder.get_insert_block().unwrap()));
            self.builder.build_unconditional_branch(cont_bb).unwrap();

            self.builder.position_at_end(next_bb);
        }

        // Err(DiscriminantError.OutOfRange { value: <raw> }) — reached when no
        // declared value matched.
        let field = FieldInit {
            name: "value".to_string(),
            value: raw_ident.clone(),
            shorthand: false,
            span: *span,
        };
        let payload = self.compile_enum_struct_variant_init(
            "DiscriminantError",
            "OutOfRange",
            std::slice::from_ref(&field),
        )?;
        let err_slot = format!("__karac_tryfrom_err_{enum_name}");
        let err_alloca = self.create_entry_alloca(fn_val, &err_slot, payload.get_type());
        self.builder.build_store(err_alloca, payload).unwrap();
        self.variables.insert(
            err_slot.clone(),
            VarSlot {
                ptr: err_alloca,
                ty: payload.get_type(),
            },
        );
        let err_arg = CallArg {
            label: None,
            mut_marker: false,
            mut_marker_span: None,
            value: Expr {
                kind: ExprKind::Identifier(err_slot),
                span: *span,
            },
            span: *span,
        };
        let err_val = self
            .try_compile_enum_variant("Err", Some("Result"), std::slice::from_ref(&err_arg))?
            .ok_or_else(|| "enum try_from: failed to build Err(DiscriminantError…)".to_string())?;
        incoming.push((err_val, self.builder.get_insert_block().unwrap()));
        self.builder.build_unconditional_branch(cont_bb).unwrap();

        self.builder.position_at_end(cont_bb);
        let phi = self
            .builder
            .build_phi(incoming[0].0.get_type(), "enumtryfrom.res")
            .unwrap();
        for (val, bb) in &incoming {
            phi.add_incoming(&[(val, *bb)]);
        }
        Ok(Some(phi.as_basic_value()))
    }

    pub(super) fn compile_refinement_try_from(
        &mut self,
        rname: &str,
        arg: &Expr,
        span: &Span,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        if !self
            .contract_state
            .refinement_predicates
            .contains_key(rname)
        {
            return Ok(None);
        }
        let value = self.compile_expr(arg)?;
        self.bind_refine_self(value, rname);
        let cond = self.compile_bound_predicate(rname)?;

        let fn_val = self
            .current_fn
            .ok_or_else(|| "refinement try_from emitted outside a function".to_string())?;
        let ok_bb = self.context.append_basic_block(fn_val, "tryfrom.ok");
        let err_bb = self.context.append_basic_block(fn_val, "tryfrom.err");
        let cont_bb = self.context.append_basic_block(fn_val, "tryfrom.cont");
        self.builder
            .build_conditional_branch(cond, ok_bb, err_bb)
            .unwrap();

        // Ok(value) — reference the stored value via the synthetic local so
        // `arg` is not re-evaluated (it may carry side effects).
        self.builder.position_at_end(ok_bb);
        let ok_arg = CallArg {
            label: None,
            mut_marker: false,
            mut_marker_span: None,
            value: Expr {
                kind: ExprKind::Identifier(REFINE_SELF.to_string()),
                span: *span,
            },
            span: *span,
        };
        let ok_val = self
            .try_compile_enum_variant("Ok", Some("Result"), std::slice::from_ref(&ok_arg))?
            .ok_or_else(|| "failed to build Ok(...) for refinement try_from".to_string())?;
        // `try_from` CONSUMES its argument: on the Ok path the heap buffer
        // (`Vec`/`String`) now lives in the `Ok` payload, so the source
        // binding (`enriched` / `v`) must NOT free it again at scope exit —
        // else a double-free against the `Ok` payload's drop (the Weave
        // dogfood's `NonEmpty.try_from(enriched)`). The suppression emits a
        // `store cap = 0` at the current insert point, so placing it in the OK
        // block makes it branch-local: on the Err path the value is discarded
        // and the source's own cleanup (cap intact) correctly frees it.
        self.suppress_source_vec_cleanup_for_arg(arg);
        let ok_end = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(cont_bb).unwrap();

        // Err(message)
        self.builder.position_at_end(err_bb);
        let err_arg = CallArg {
            label: None,
            mut_marker: false,
            mut_marker_span: None,
            value: Expr {
                kind: ExprKind::StringLit(format!("value does not satisfy refinement `{rname}`")),
                span: *span,
            },
            span: *span,
        };
        let err_val = self
            .try_compile_enum_variant("Err", Some("Result"), std::slice::from_ref(&err_arg))?
            .ok_or_else(|| "failed to build Err(...) for refinement try_from".to_string())?;
        let err_end = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(cont_bb).unwrap();

        // Merge the two arms.
        self.builder.position_at_end(cont_bb);
        self.unbind_refine_self();
        let phi = self
            .builder
            .build_phi(ok_val.get_type(), "tryfrom.result")
            .unwrap();
        phi.add_incoming(&[(&ok_val, ok_end), (&err_val, err_end)]);
        Ok(Some(phi.as_basic_value()))
    }
}
