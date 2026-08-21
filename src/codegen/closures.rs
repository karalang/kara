//! Closure compilation: literal capture, env-struct emission, indirect
//! calls, and the free-variable scan helpers.
//!
//! Houses `closure_value_type` (the `{fn_ptr, env_ptr}` fat-pointer
//! struct), `compile_closure` (the synthesized closure-body fn +
//! caller-side env capture), `compile_closure_call` (indirect call
//! through a closure binding), `infer_closure_return_type`, and the
//! `collect_closure_free_vars` / `refs_in_expr` / `refs_in_block`
//! free-variable scan helpers consumed by both closure capture and
//! par-block capture sets.

use crate::ast::*;
use crate::ownership::CapturePath;
use crate::resolver::SpanKey;
use crate::token::Span;
use std::collections::{HashMap, HashSet};

use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum, FunctionType, StructType};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, FunctionValue};
use inkwell::AddressSpace;

use super::state::VarSlot;

/// Per-root unpack plan for the disjoint-capture slice-4 per-path env
/// layout. Records how a captured root binding is rebuilt inside the
/// synthesized closure body from one or more env-struct slots.
///
/// `whole_root_slot = Some(idx)` means the env's slot at `idx` holds the
/// entire root value (matches today's per-name layout); the body unpack
/// loads the slot and stores into a root-named alloca, and field accesses
/// in the body walk it normally.
///
/// `whole_root_slot = None` means the root was captured *path-precisely*:
/// `sub_slots` lists the env slots that hold leaf values at non-empty
/// projection chains under this root. The body unpack allocates a fresh
/// root-typed alloca (uninit'd in the unread fields — the ownership pass
/// guarantees the body never reads them) and writes each sub-slot leaf
/// into its GEP chain. The body's field accesses then walk the stitched
/// root as if it were a whole-root capture.
struct RootUnpackPlan<'ctx> {
    /// LLVM type of the root in the outer scope (matches `VarSlot.ty`).
    root_ty: BasicTypeEnum<'ctx>,
    /// Source type-name of the root, if `var_type_names` has an entry.
    /// Propagated into the closure body's `var_type_names` so method
    /// dispatch on the captured root resolves through the user impl-block.
    type_name: Option<String>,
    /// `Some(env_slot_idx)` → whole-root capture; `None` → per-path.
    whole_root_slot: Option<usize>,
    /// Per-sub-path entries when `whole_root_slot` is None. Each tuple
    /// is `(env_slot_idx, gep_chain, leaf_ty)` — load env[idx] of type
    /// `leaf_ty`, then GEP into the root alloca via `gep_chain` and store.
    sub_slots: Vec<(usize, Vec<u32>, BasicTypeEnum<'ctx>)>,
}

/// Full per-closure capture layout — slot list (env struct field order)
/// plus the per-root unpack plans. Produced by
/// `Codegen::build_capture_path_layout` when ownership data is available
/// for the closure's `SpanKey` and every captured root resolves cleanly
/// through `struct_field_names` / `struct_field_type_names`. `None` →
/// fall back to the legacy `collect_closure_free_vars` per-name layout.
struct CapturePathLayout<'ctx> {
    /// Env-struct field types in slot order. Empty when no captures.
    slot_tys: Vec<BasicTypeEnum<'ctx>>,
    /// `slot_idx → (root_name, gep_chain)` — drives capture-site loads:
    /// for slot i, load `outer.variables[root]` via the gep chain and
    /// store into env field i. Empty `gep_chain` → store the whole-root
    /// value.
    slot_sources: Vec<(String, Vec<u32>)>,
    /// Per-root unpack plans, in deterministic root-name order. Drives
    /// the closure body's prelude.
    root_plans: Vec<(String, RootUnpackPlan<'ctx>)>,
}

impl<'ctx> super::Codegen<'ctx> {
    // ── Closure compilation ────────────────────────────────────────

    /// The LLVM struct type used to represent a closure fat-pointer: `{ ptr fn_ptr, ptr env_ptr }`.
    pub(super) fn closure_value_type(&self) -> StructType<'ctx> {
        let ptr = self.context.ptr_type(AddressSpace::default());
        self.context.struct_type(&[ptr.into(), ptr.into()], false)
    }

    // ── Escape analysis (B-2026-08-16-13) ──────────────────────────
    //
    // The escaping-closure validators, the four heap-env producer-set
    // fixpoints, and their helper walks moved VERBATIM to
    // `crate::closure_escape` — a plain-AST crate-level module shared with
    // the `escaping_closure` check-time lint, so the build gate and the
    // check diagnostic can never drift (see the module doc there and the
    // B-2026-08-16-13 ledger row). `compile` builds the producer sets once
    // via `EscapeAnalysis::compute`; `compile_function` runs
    // `EscapeAnalysis::check_function` per function. The state lives in
    // `self.closure_state.escape`; the shims below keep emission call sites
    // reading through `self.…` as before.

    /// `true` when `e` is a call that RETURNS a heap-env closure — a call to
    /// a `fns_returning_heap_env` free fn or to a curry local. Delegates to
    /// the shared escape analysis.
    pub(super) fn is_heap_env_producing_call(&self, e: &Expr) -> bool {
        self.closure_state.escape.is_heap_env_producing_call(e)
    }

    /// Span of `func`'s sanctioned tail capturing-closure literal (Slice 1),
    /// if any. Delegates to the shared escape analysis.
    pub(super) fn func_tail_heap_closure_span(&self, func: &Function) -> Option<(usize, usize)> {
        self.closure_state.escape.func_tail_heap_closure_span(func)
    }

    /// Span of a closure body's sanctioned tail capturing-closure literal
    /// (currying, B-2026-07-12-12). Delegates to the shared escape analysis.
    pub(super) fn closure_tail_heap_closure_span(
        &self,
        outer_params: &[ClosureParam],
        outer_body: &Expr,
    ) -> Option<(usize, usize)> {
        self.closure_state
            .escape
            .closure_tail_heap_closure_span(outer_params, outer_body)
    }

    /// Build the LLVM function type for the env-first closure-call ABI of a
    /// surface `Fn(P0, P1, …) -> R` annotation: `R (ptr env, P0, P1, …)`. The
    /// leading `ptr` is the captured-environment pointer every closure body
    /// (and every reified-fn trampoline) receives as its first parameter; a
    /// missing / `unit` return lowers to `void` (mirroring `declare_function`
    /// for a no-return fn, and matched by `compile_closure_call`'s void arm).
    ///
    /// Used both to register a `Fn`-typed parameter in `closure_fn_types` (so a
    /// body call `f(x)` becomes an indirect call) and to type the synthesized
    /// trampoline in `reify_named_fn_as_fn_value` — building both from the same
    /// annotation guarantees the indirect-call signature and the trampoline
    /// signature agree (B-2026-06-20-1).
    pub(super) fn closure_abi_fn_type(
        &self,
        params: &[TypeExpr],
        return_type: Option<&TypeExpr>,
    ) -> FunctionType<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let mut param_tys: Vec<BasicMetadataTypeEnum<'ctx>> = vec![ptr_ty.into()];
        for t in params {
            param_tys.push(BasicMetadataTypeEnum::from(self.llvm_type_for_type_expr(t)));
        }
        // A `unit` return (`Fn(..) -> ()`, or the typechecker's `Type::Unit`
        // round-tripped to `TypeKind::Unit`) is `void`, matching how a
        // no-return target lowers and how `compile_closure_call` treats a void
        // result — without this it would lower to `i64` and mismatch.
        if return_type.is_none() || matches!(return_type.map(|t| &t.kind), Some(TypeKind::Unit)) {
            return self.context.void_type().fn_type(&param_tys, false);
        }
        match return_type.map(|t| self.llvm_type_for_type_expr(t)) {
            Some(BasicTypeEnum::IntType(t)) => t.fn_type(&param_tys, false),
            Some(BasicTypeEnum::FloatType(t)) => t.fn_type(&param_tys, false),
            Some(BasicTypeEnum::PointerType(t)) => t.fn_type(&param_tys, false),
            Some(BasicTypeEnum::StructType(t)) => t.fn_type(&param_tys, false),
            Some(BasicTypeEnum::ArrayType(t)) => t.fn_type(&param_tys, false),
            Some(BasicTypeEnum::VectorType(t)) => t.fn_type(&param_tys, false),
            Some(BasicTypeEnum::ScalableVectorType(_)) | None => {
                self.context.void_type().fn_type(&param_tys, false)
            }
        }
    }

    /// B-2026-06-20-1: reify a bare named `fn` passed in `Fn(...)`-typed
    /// argument position into a closure fat-pointer value `{ trampoline, null
    /// env }`, so it dispatches through the same env-first indirect-call ABI as
    /// a closure literal. Returns `None` (caller compiles the arg normally)
    /// unless `arg` is a bare identifier that names a free fn AND the callee's
    /// parameter `idx` is `Fn(...)`-typed.
    ///
    /// A bare fn name otherwise lowers to a raw `ptr` (`@doubler`), which fails
    /// LLVM module verification against the fat-pointer parameter slot.
    pub(super) fn reify_named_fn_as_fn_value(
        &mut self,
        callee: &str,
        idx: usize,
        arg: &Expr,
    ) -> Option<BasicValueEnum<'ctx>> {
        let ExprKind::Identifier(fn_name) = &arg.kind else {
            return None;
        };
        // Gate: the callee's parameter `idx` must be `Fn(...)`-typed. (The
        // shared `reify_named_fn_value` separately rejects a name shadowed by a
        // higher-precedence binding.)
        let is_fn_param = self
            .fn_sig
            .fn_asts
            .get(callee)
            .and_then(|f| f.params.get(idx))
            .is_some_and(|p| matches!(p.ty.kind, TypeKind::FnType { .. }));
        if !is_fn_param {
            return None;
        }
        self.reify_named_fn_value(fn_name).map(|(fat, _)| fat)
    }

    /// B-2026-06-21-1: reify a bare identifier that names a free `fn` into a
    /// closure fat-pointer value `{ trampoline, null env }` plus the env-first
    /// `FunctionType` of that trampoline (for `closure_fn_types`). Shared by the
    /// `Fn`-typed argument-site reify above and the `let f = some_fn` binding
    /// path (`compile_stmt`), so a fn value works whether passed directly,
    /// bound to a local first, or called through that local.
    ///
    /// Returns `None` unless `name` resolves to a module function and is not
    /// shadowed by a higher-precedence binding — mirroring the resolution order
    /// of `compile_expr`'s `Identifier` arm (const-subst / local / module `let`
    /// / unit enum variant / top-level const all win over a free fn).
    pub(super) fn reify_named_fn_value(
        &mut self,
        name: &str,
    ) -> Option<(BasicValueEnum<'ctx>, FunctionType<'ctx>)> {
        if !self.name_resolves_to_free_fn(name) {
            return None;
        }
        let target = self.module.get_function(name)?;
        let tramp_name = format!("__karac_fnval_{}", name);
        let tramp = match self.module.get_function(&tramp_name) {
            Some(t) => t,
            None => self.emit_fn_value_trampoline(&tramp_name, target),
        };

        // Build the fat pointer `{ trampoline_ptr, null }` — null env because a
        // free fn captures nothing.
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let fat_ty = self.closure_value_type();
        let mut fat = fat_ty.get_undef();
        fat = self
            .builder
            .build_insert_value(
                fat,
                tramp.as_global_value().as_pointer_value(),
                0,
                "fnval_fn",
            )
            .unwrap()
            .into_struct_value();
        fat = self
            .builder
            .build_insert_value(fat, ptr_ty.const_null(), 1, "fnval_env")
            .unwrap()
            .into_struct_value();
        Some((fat.into(), tramp.get_type()))
    }

    /// `true` when `name` would resolve to a free `fn` in `compile_expr`'s
    /// `Identifier` arm — i.e. it names a module function and is NOT shadowed by
    /// any higher-precedence binding (const-generic subst, local, module `let`,
    /// unit enum variant, or top-level `const`). Side-effect free: it inspects
    /// membership tables only, never `try_load_module_binding` /
    /// `try_unit_enum_variant` (those emit IR).
    fn name_resolves_to_free_fn(&self, name: &str) -> bool {
        if self.variables.contains_key(name)
            || self.mono_state.const_subst.contains_key(name)
            || self.mod_bindings.consts.contains_key(name)
            || self.mod_bindings.module_bindings.contains_key(name)
        {
            return false;
        }
        // Unit enum variant (zero payload fields under some enum layout).
        let is_unit_variant = self.type_decls.enum_layouts.values().any(|layout| {
            layout.tags.contains_key(name)
                && layout.field_counts.get(name).copied().unwrap_or(0) == 0
        });
        if is_unit_variant {
            return false;
        }
        self.module.get_function(name).is_some()
    }

    /// The env-first closure-call ABI `FunctionType` for `target`:
    /// `R (ptr env, P0, P1, …)` — `target`'s own signature with a leading env
    /// pointer prepended. This is the type of `target`'s reify trampoline and
    /// the `closure_fn_types` entry for any binding that holds `target` as a
    /// fn value.
    fn env_first_fn_type(&self, target: FunctionValue<'ctx>) -> FunctionType<'ctx> {
        let target_ty = target.get_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let mut param_tys: Vec<BasicMetadataTypeEnum<'ctx>> = vec![ptr_ty.into()];
        // `FunctionType::get_param_types` already yields `BasicMetadataTypeEnum`.
        param_tys.extend(target_ty.get_param_types());
        match target_ty.get_return_type() {
            Some(BasicTypeEnum::IntType(t)) => t.fn_type(&param_tys, false),
            Some(BasicTypeEnum::FloatType(t)) => t.fn_type(&param_tys, false),
            Some(BasicTypeEnum::PointerType(t)) => t.fn_type(&param_tys, false),
            Some(BasicTypeEnum::StructType(t)) => t.fn_type(&param_tys, false),
            Some(BasicTypeEnum::ArrayType(t)) => t.fn_type(&param_tys, false),
            Some(BasicTypeEnum::VectorType(t)) => t.fn_type(&param_tys, false),
            Some(BasicTypeEnum::ScalableVectorType(_)) | None => {
                self.context.void_type().fn_type(&param_tys, false)
            }
        }
    }

    /// If `let <name>[: ty] = value` binds a first-class fn value, return the
    /// env-first closure `FunctionType` to register in `closure_fn_types` (so a
    /// later `name(args)` lowers to an indirect call through the fat pointer).
    /// Recognizes, in precedence order: an explicit `Fn(...)` / `OnceFn(...)`
    /// annotation; a bare free-fn-name RHS; and a call whose callee's declared
    /// return type is `Fn(...)` (so `let f = pick()` where `pick -> Fn(..)`
    /// works un-annotated). Returns `None` when the binding is not a fn value,
    /// or when its signature can't be recovered at this layer — e.g. an
    /// un-annotated field/index read of a `Fn(..)` value, which still needs an
    /// explicit `let g: Fn(..) = h.f;` annotation (B-2026-06-21-2 residual).
    pub(super) fn let_binding_fn_value_type(
        &self,
        ty: Option<&TypeExpr>,
        value: &Expr,
    ) -> Option<FunctionType<'ctx>> {
        if let Some(TypeKind::FnType {
            params,
            return_type,
            ..
        }) = ty.map(|t| &t.kind)
        {
            return Some(self.closure_abi_fn_type(params, return_type.as_deref()));
        }
        if let ExprKind::Identifier(n) = &value.kind {
            if self.name_resolves_to_free_fn(n) {
                return self
                    .module
                    .get_function(n)
                    .map(|t| self.env_first_fn_type(t));
            }
        }
        if let ExprKind::Call { callee, .. } = &value.kind {
            if let ExprKind::Identifier(callee_name) = &callee.kind {
                if let Some(TypeKind::FnType {
                    params,
                    return_type,
                    ..
                }) = self
                    .fn_sig
                    .fn_return_type_exprs
                    .get(callee_name)
                    .map(|t| &t.kind)
                {
                    return Some(self.closure_abi_fn_type(params, return_type.as_deref()));
                }
            }
        }
        // General fallback (B-2026-06-21-3): the typechecker typed the RHS
        // expression as a function — recover its `FnType` from the lowering
        // pass's `fn_value_typed_exprs` span table. Covers an un-annotated fn
        // value read from a struct field (`let g = h.f`), a `Vec[Fn]` element
        // (`let g = v[0]`), a method call, etc. — any inferred fn-value binding
        // whose RHS shape the cases above don't special-case.
        if let Some(TypeKind::FnType {
            params,
            return_type,
            ..
        }) = self
            .span_tables
            .fn_value_typed_exprs
            .get(&(value.span.offset, value.span.length))
            .map(|t| &t.kind)
        {
            return Some(self.closure_abi_fn_type(params, return_type.as_deref()));
        }
        None
    }

    /// Synthesize a per-fn env-ignoring trampoline `__karac_fnval_<name>` whose
    /// signature is the env-first wrap of `target`'s own signature
    /// (`R (ptr env, P0, P1, …)`): it drops the leading env pointer and forwards
    /// the remaining args to `target`, returning its result. This lets a plain
    /// free fn (whose real signature has no env parameter) be invoked through
    /// the same indirect-call shape as a closure body. Deriving the signature
    /// from `target` (not from a `Fn(...)` annotation) keeps the one memoized
    /// `__karac_fnval_<name>` definition consistent across every reify site.
    /// Memoized by the caller via `module.get_function`.
    fn emit_fn_value_trampoline(
        &mut self,
        tramp_name: &str,
        target: FunctionValue<'ctx>,
    ) -> FunctionValue<'ctx> {
        let saved_bb = self.builder.get_insert_block();
        let tramp_ty = self.env_first_fn_type(target);
        let tramp = self.module.add_function(tramp_name, tramp_ty, None);
        let entry = self.context.append_basic_block(tramp, "entry");
        self.builder.position_at_end(entry);

        // Forward the user args (params 1..) to the target; param 0 (env) is
        // ignored — a free fn captures nothing.
        let fwd: Vec<BasicMetadataValueEnum<'ctx>> = tramp
            .get_params()
            .into_iter()
            .skip(1)
            .map(BasicMetadataValueEnum::from)
            .collect();
        let call = self.builder.build_call(target, &fwd, "fnval_fwd").unwrap();
        let ret_val = call.try_as_basic_value();
        if target.get_type().get_return_type().is_some() && !ret_val.is_instruction() {
            self.builder
                .build_return(Some(&ret_val.unwrap_basic()))
                .unwrap();
        } else {
            self.builder.build_return(None).unwrap();
        }

        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        tramp
    }

    /// Compile `|params| body` into a fat-pointer value `{ fn_ptr, env_ptr }`.
    ///
    /// Sets `pending_closure_fn_type` so the surrounding `let` binding can register the
    /// function type for later indirect calls.
    ///
    /// `closure_span` is the `ExprKind::Closure` expression's own span — used
    /// as the lookup key into `Codegen::closure_capture_paths` (sourced from
    /// `OwnershipCheckResult::closure_capture_path_modes`). When the ownership
    /// pass supplied per-path mode data for this closure and every captured
    /// root resolves cleanly through `struct_field_names`, the env struct is
    /// laid out with one field per captured path (disjoint-capture slice 4);
    /// otherwise the legacy per-captured-name layout from
    /// `collect_closure_free_vars` is used.
    pub(super) fn compile_closure(
        &mut self,
        params: &[ClosureParam],
        body: &Expr,
        closure_span: &Span,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let id = self.closure_state.closure_counter;
        self.closure_state.closure_counter += 1;
        let fn_name = format!("__closure_{}", id);

        // 1. Collect free variables (names referenced in body, not in
        //    params, present in scope). Always run the per-name walker —
        //    it doubles as the fallback when no per-path layout is
        //    available, and the per-path layout consults it indirectly
        //    via `self.variables` for the root types.
        let free_vars = self.collect_closure_free_vars(params, body);

        // 1a. `mut ref` closure capture (B-2026-07-11-23): a stored closure
        //     VALUE whose body MUTATES a captured name captures that name BY
        //     REFERENCE (design.md Rule 2) — the write must land on the OUTER
        //     binding, not an env copy. Codegen captures such a name as a
        //     POINTER to its outer slot (env field is `ptr`, the body reads and
        //     writes through it), so mutations propagate to the real slot and
        //     the interpreter's shared-cell semantics are matched (`|x|{c=c+x}`
        //     over `f(3); f(4)` yields 7 on both engines).
        //
        //     Soundness: a by-reference capture is valid only while the outer
        //     slot outlives the closure — i.e. for a NON-escaping closure. An
        //     ESCAPING (returned → heap-env) closure would dangle, so it is
        //     still refused. `reject_escaping_capturing_closure`
        //     (`compile_function`) already rejects every OTHER escape route
        //     (return-of-a-non-tail, struct field, collection store, id chain),
        //     so a closure reaching here with `is_heap_env == false` provably
        //     does not escape its frame. The inlined `fold`/`any`/`all`/`sum`
        //     terminals never build a closure value, so they never reach here.
        let is_heap_env = self
            .fn_ctx
            .current_fn_heap_closure_spans
            .contains(&(closure_span.offset, closure_span.length));
        let mutref_caps: HashSet<String> = {
            let mut assigned: HashSet<String> = HashSet::new();
            collect_assigned_roots_expr(body, &mut assigned);
            // B-2026-07-15-13: a captured COLLECTION mutated through a mutating
            // METHOD (`acc.push(x)`, `m.insert(k, v)`) must ALSO capture by
            // mut-ref so the mutation writes through to the outer binding — the
            // method-mutation sibling of the direct-assignment /
            // `acc[i] = ...` (index-assign root) detection above.
            // `collect_assigned_roots_expr` never marks a method receiver, so a
            // closure that pushes to a captured Vec silently mutated a by-value
            // COPY under codegen (`acc.len()` read back 0), while the
            // interpreter — whose Vec is reference-semantics — mutated through.
            // Restricted to a curated set of DEFINITELY-mutating built-in
            // collection methods so a read-only receiver (`len`/`get`/`iter`)
            // never over-marks — over-marking would wrongly force a by-ref
            // capture and trip the escaping-closure rejection just below.
            collect_mut_method_receiver_roots_expr(body, &mut assigned);
            free_vars
                .iter()
                .filter(|n| assigned.contains(n.as_str()))
                .cloned()
                .collect()
        };
        if !mutref_caps.is_empty() && is_heap_env {
            let name = mutref_caps.iter().next().unwrap();
            return Err(format!(
                "a stored closure that BOTH mutates the captured variable `{name}` (`mut ref` \
                 capture, design.md Rule 2) AND escapes its defining function (returned) is not \
                 yet supported under `karac build`: the by-reference capture would outlive the \
                 frame that owns `{name}`. Re-run with `--interp` (or `KARAC_RUN_JIT=0`), or \
                 thread the mutated state through the closure's parameters / return value instead."
            ));
        }

        // 1b. Disjoint-capture slice 4: per-path env layout when the
        //     ownership pass supplied modes for this closure and every
        //     captured root resolves cleanly. Falls back to per-name
        //     layout when the data is missing (e.g., `compile_to_ir`
        //     called without ownership) or any captured root has a
        //     projection step that can't be resolved (treated as a
        //     whole-root capture for that root inside the path layout
        //     builder).
        //
        //     A `mut ref` capture (1a) forces the per-name layout: its env
        //     slot is a POINTER to the outer root (not a value / sub-field),
        //     which the per-path struct-field-precise layout does not model.
        // Slice 2 (B-2026-06-22-2): an escaping (heap-env) closure that captures
        // a WHOLE String / Vec value takes the per-name env layout so the
        // move-out cap-suppression + env-drop synthesis below can key off each
        // captured binding by name. A disjoint sub-field (`path_layout`) capture
        // of a heap field stays rejected by the heap-capture gate (it needs the
        // source aggregate's own drop coordinated, which is a later slice).
        let vec_ty_basic: BasicTypeEnum<'ctx> = self.vec_struct_type().into();
        let has_heap_capture = is_heap_env
            && free_vars
                .iter()
                .any(|n| self.variables.get(n).is_some_and(|s| s.ty == vec_ty_basic));
        let path_layout = if mutref_caps.is_empty() && !has_heap_capture {
            self.build_capture_path_layout(closure_span, &free_vars)
        } else {
            None
        };

        // The original (value) type of each per-name capture, aligned with
        // `free_vars`. A `mut ref` capture stores a `ptr` to the outer slot in
        // the env, but the body still binds the var at its real type `T` (reads
        // / writes go through the pointer), so keep `T` here for the body's
        // `VarSlot.ty`. Only populated on the per-name path.
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let orig_cap_tys: Vec<BasicTypeEnum<'ctx>> = if path_layout.is_none() {
            free_vars.iter().map(|n| self.variables[n].ty).collect()
        } else {
            Vec::new()
        };

        // 2. Build the env struct type: { T0_cap, T1_cap, ... }.
        //    Use a dummy i8 when there are no captures so we always have
        //    a valid struct type. A `mut ref`-captured name's slot is a `ptr`
        //    to its outer binding (by-reference capture, 1a).
        let env_field_types: Vec<BasicTypeEnum<'ctx>> = if let Some(layout) = path_layout.as_ref() {
            if layout.slot_tys.is_empty() {
                vec![self.context.i8_type().into()]
            } else {
                layout.slot_tys.clone()
            }
        } else if free_vars.is_empty() {
            vec![self.context.i8_type().into()]
        } else {
            free_vars
                .iter()
                .zip(orig_cap_tys.iter())
                .map(|(n, &t)| {
                    if mutref_caps.contains(n) {
                        ptr_ty.into()
                    } else {
                        t
                    }
                })
                .collect()
        };
        let env_struct_ty = self.context.struct_type(&env_field_types, false);

        // Slice 1 (B-2026-06-22-2): does this closure escape its defining
        // function via the return? If so its environment must outlive the
        // frame, so it is allocated as a reference-counted HEAP box
        // `{ i64 refcount, <env_struct> }` instead of a stack alloca. The
        // closure body GEPs past the refcount to reach the payload; the
        // owning caller binding frees it (refcount dec) at scope exit.
        // (`is_heap_env` is computed in 1a — an escaping closure with a
        // `mut ref` capture was already refused there.)
        if is_heap_env {
            // Slice 2 (B-2026-06-22-2): POD captures (Slice 1) AND whole
            // String / Vec captures are supported. A String/Vec capture is
            // OWNED by the env (the ownership pass promotes the escaping
            // read-only capture to an `Own` move) — an env-drop fn frees its
            // buffer when the RC box hits zero, and the source binding's `cap`
            // is zeroed at the capture site so it does not double-free. Any
            // OTHER heap shape — a `shared` / Map / Set / raw pointer, or a
            // by-value struct/tuple (including a disjoint `path_layout` heap
            // sub-field, which is why a `StructType` field is only accepted on
            // the per-name path, `path_layout.is_none()`) — still needs its own
            // drop wiring, so reject it rather than leak / double-free it.
            let vec_ty = self.vec_struct_type();
            let allow_heap = path_layout.is_none();
            // On the per-name path `env_field_types[i]` corresponds to
            // `free_vars[i]`. A `String`/`Vec` field is accepted only when its
            // ELEMENT is POD (`String`'s bytes are `i8`; `Vec[i64]`'s are `i64`):
            // a Vec-of-heap (`Vec[String]` / `Vec[Vec]` / `Vec[shared]`) needs
            // per-element deep clone + drop that the shallow env clone/free can't
            // provide (its elements would UAF on read after the source deep-frees,
            // or leak), so it stays rejected for now.
            let supported = env_field_types.iter().enumerate().all(|(i, t)| match t {
                BasicTypeEnum::IntType(_) | BasicTypeEnum::FloatType(_) => true,
                BasicTypeEnum::StructType(st) if allow_heap && *st == vec_ty => free_vars
                    .get(i)
                    .and_then(|n| self.var_types.vec_elem_types.get(n).copied())
                    .is_some_and(|et| {
                        matches!(et, BasicTypeEnum::IntType(_) | BasicTypeEnum::FloatType(_))
                    }),
                _ => false,
            });
            if !supported {
                return Err(
                    "error[E_ESCAPING_CLOSURE_HEAP_CAPTURE_NOT_YET]: returning a closure \
                     that captures a non-POD, non-String/Vec value (shared / Map / Set / raw \
                     pointer / by-value struct or tuple) is not yet supported — POD (integer / \
                     float / bool) and whole String / Vec captures can be returned today \
                     (heap-closure-environment epic B-2026-06-22-2, Slice 2). Workaround: pass \
                     the closure down by a `Fn(..)` parameter instead of returning it."
                        .to_string(),
                );
            }
        }
        // Box layout `{ i64 refcount, ptr env_drop, <env_struct> }`. The env
        // payload is field 2; field 1 holds the per-closure env-drop fn (Slice 2,
        // B-2026-06-22-2) — a FIXED offset (8) so the generic
        // `emit_heap_closure_env_dec` can load + call it via a `{ i64, ptr }`
        // prefix GEP without knowing the variable-size env struct. `null` when
        // the env owns no heap (POD-only Slice 1 closures).
        let env_box_ty = self.context.struct_type(
            &[
                self.context.i64_type().into(),
                self.context.ptr_type(AddressSpace::default()).into(),
                env_struct_ty.into(),
            ],
            false,
        );

        // 3. Determine param types. Source annotation wins, otherwise consult
        //    `pending_closure_param_hints` (caller pushdown — e.g. `Vec.sort_by`
        //    handing the element type to a `|a, b|` comparator), otherwise the
        //    typechecker's inferred `Fn(...)` type at the closure's own span
        //    (`fn_value_typed_exprs`, populated by the lowering pass from
        //    `expr_types` — contextual inference from the callee's declared
        //    `Fn` param covers the common un-annotated arg), otherwise fall
        //    back to i64. B-2026-07-02-12: before the span fallback, an
        //    un-annotated `|a| f"{a}!"` passed to a `Fn(String) -> String`
        //    param compiled as `(ptr, i64) -> i64` while the call site
        //    dispatched through the declared-`Fn` ABI — an indirect-call
        //    signature mismatch that silently printed the String's pointer
        //    word as an integer.
        let param_hints = self.closure_state.pending_closure_param_hints.take();
        let inferred_fn_te = self
            .span_tables
            .fn_value_typed_exprs
            .get(&(closure_span.offset, closure_span.length))
            .cloned();
        let inferred_param_tes: Vec<Option<TypeExpr>> = match inferred_fn_te.as_ref() {
            Some(TypeExpr {
                kind: TypeKind::FnType { params: ps, .. },
                ..
            }) => ps.iter().map(|te| Some(te.clone())).collect(),
            _ => Vec::new(),
        };
        let param_llvm_types: Vec<BasicTypeEnum<'ctx>> = params
            .iter()
            .enumerate()
            .map(|(i, p)| {
                if let Some(te) = p.ty.as_ref() {
                    return self.llvm_type_for_type_expr(te);
                }
                if let Some(hints) = param_hints.as_ref() {
                    if let Some(&hinted) = hints.get(i) {
                        return hinted;
                    }
                }
                if let Some(Some(te)) = inferred_param_tes.get(i) {
                    return self.llvm_type_for_type_expr(te);
                }
                self.context.i64_type().into()
            })
            .collect();

        // 4. Infer return type from the body expression.
        //
        // This map answers "what does an occurrence of this NAME evaluate to
        // inside the body", which for a borrow param is NOT its slot type. A
        // `ref T` / `mut ref T` param's slot holds a POINTER, but every read of
        // it in the body goes through `load_variable`, which derefs — so the
        // body's value type is the POINTEE. Handing the raw `ptr` to the
        // return-type heuristic declared the closure `-> ptr` while the body
        // returned `T`, and LLVM module verification rejected the function
        // ("Function return type does not match operand type of return inst").
        //
        // B-2026-08-08-30: that is what `Vec[i64].first().map(|x| x + 1)` hit.
        // The `Binary` arm returns the left operand's type and the `Identifier`
        // arm returned `ptr`, so the declared return was `ptr` against a body
        // computing `i64`. It only ever surfaced for a payload whose mapper
        // RESULT type is the payload type: `|x| x > 5` returns `bool` and `|x|
        // x + 1.0` hits the float arm, so both were fine, which is why the
        // family read as working. `Vec[T].first()/get()` are the sources of
        // borrowed payloads — `Map.get` yields an OWNED `Option[V]` and never
        // reached this.
        //
        // Only the return-type heuristic is corrected here. The param ABI is
        // `param_llvm_types` above and stays a pointer, which is what the
        // caller passes and what `load_variable`'s deref expects.
        let closure_param_types: HashMap<String, BasicTypeEnum<'ctx>> = params
            .iter()
            .enumerate()
            .zip(param_llvm_types.iter())
            .filter_map(|((i, cp), ty)| {
                let PatternKind::Binding(n) = &cp.pattern.kind else {
                    return None;
                };
                let effective_te = cp
                    .ty
                    .as_ref()
                    .or_else(|| inferred_param_tes.get(i).and_then(Option::as_ref));
                let value_ty = effective_te
                    .and_then(|te| self.inner_type_of_ref(te))
                    .unwrap_or(*ty);
                Some((n.clone(), value_ty))
            })
            .collect();
        // Return type: the structural heuristic `infer_closure_return_type` is
        // correct and usage-specific for most bodies (e.g. an `a.cmp(b)`
        // Ordering result that `sort_by` extracts a tag from), so it is the
        // default. But it CANNOT resolve a method-call body whose returned
        // SURFACE type it can't recover from LLVM types alone — `|s|
        // s.to_uppercase()` returns a String (String/Vec share one LLVM type),
        // and `|x| x.mul(x)` / `|q| q.twice()` return a user struct/tuple — so
        // the heuristic falls back to `i64` and the body's aggregate return
        // mismatches the declared `i64` (LLVM "return type does not match operand
        // type"). Override when the typechecker-recorded `Fn(T) -> R` says the
        // return is an AGGREGATE (`vec_struct` / user struct / tuple) but the
        // heuristic gave up to the scalar `i64`: trust the recorded type. A
        // fieldless enum return (`a.cmp(b)` Ordering→i64-tag) lowers to an int,
        // not a struct, so it stays on the heuristic — no regression.
        // Heap-payload map codegen (B-2026-07-12-11); struct/tuple returns
        // (B-2026-07-19, autograd `Tape.grad` closures).
        let heuristic_rt = self.infer_closure_return_type(body, &closure_param_types);
        let return_ty = match inferred_fn_te.as_ref() {
            Some(TypeExpr {
                kind:
                    TypeKind::FnType {
                        return_type: Some(rt),
                        ..
                    },
                ..
            }) => {
                // B-2026-08-15-25 — the override used to require the recorded
                // type to be a STRUCT, which silently excluded every other shape
                // the heuristic cannot recover. A `bool` is `i1` — an INT — so a
                // closure returning a builtin predicate (`|s| s.starts_with(p)`,
                // `|m| m.contains_key(k)`) kept the heuristic's `i64` and the
                // body's `ret i1` failed LLVM module verification, on a program
                // `karac check` accepts and the interpreter runs.
                //
                // The gate is now the sentinel itself rather than the shape it
                // produced. `i64` IS the heuristic's "I gave up" answer (the
                // fallback arm at the bottom of `infer_closure_return_type`), so
                // whenever the typechecker recorded something else, the recorded
                // type is strictly better information — that is the same
                // reasoning the struct case already shipped on, just not
                // restricted to structs.
                //
                // A genuinely `i64`-returning body is unaffected: the recorded
                // type is `i64` too and the two agree. A fieldless enum
                // (`a.cmp(b)` → `Ordering`) never reaches here — its own arm
                // resolves the layout, so the heuristic is not `i64` for it.
                //
                // A BORROW return is excluded, and this is the one case the old
                // struct-only gate was accidentally right about. `|a| a` over a
                // borrowed payload records `ref i64`, which lowers to `ptr` —
                // but the body DEREF-ON-USES the borrow and returns the pointee,
                // so the recorded type describes the binding, not the value that
                // leaves. Taking it emitted `ret i64` against a `ptr` signature:
                // the same verifier failure this fix exists to remove, pointing
                // the other way. `infer_closure_return_type`'s own `Identifier`
                // arm already reports the POINTEE for a `ref` param
                // (B-2026-08-08-30), so the heuristic is the authority here.
                //
                // Only `Ref`/`MutRef` are excluded, not every pointer-shaped
                // record: a closure returning a handle-backed builtin (`Map`,
                // `Tensor`) also lowers to `ptr`, and there the handle IS the
                // value returned, so the record is right.
                let recorded_is_borrow = matches!(rt.kind, TypeKind::Ref(_) | TypeKind::MutRef(_));
                let recorded = self.llvm_type_for_type_expr(rt);
                if heuristic_rt == self.context.i64_type().into()
                    && recorded != heuristic_rt
                    && !recorded_is_borrow
                {
                    recorded
                } else {
                    heuristic_rt
                }
            }
            _ => heuristic_rt,
        };

        // 5. Declare the closure function: fn(ptr env_ptr, T0, T1, ...) -> R.
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let mut fn_param_types: Vec<BasicMetadataTypeEnum<'ctx>> =
            vec![BasicMetadataTypeEnum::from(ptr_ty)];
        for &ty in &param_llvm_types {
            fn_param_types.push(BasicMetadataTypeEnum::from(ty));
        }
        let fn_type = match return_ty {
            BasicTypeEnum::IntType(t) => t.fn_type(&fn_param_types, false),
            BasicTypeEnum::FloatType(t) => t.fn_type(&fn_param_types, false),
            BasicTypeEnum::PointerType(t) => t.fn_type(&fn_param_types, false),
            BasicTypeEnum::StructType(t) => t.fn_type(&fn_param_types, false),
            BasicTypeEnum::ArrayType(t) => t.fn_type(&fn_param_types, false),
            BasicTypeEnum::VectorType(t) => t.fn_type(&fn_param_types, false),
            BasicTypeEnum::ScalableVectorType(_) => {
                self.context.void_type().fn_type(&fn_param_types, false)
            }
        };
        let closure_fn = self.module.add_function(&fn_name, fn_type, None);

        // 6. Save outer codegen state — we're about to compile a new function inline.
        let saved_bb = self.builder.get_insert_block();
        let saved_fn = self.current_fn;
        let saved_vars = std::mem::take(&mut self.variables);
        let saved_var_types = std::mem::take(&mut self.var_types.var_type_names);
        let saved_loop_stack = std::mem::take(&mut self.fn_ctx.loop_stack);
        let saved_subst = std::mem::take(&mut self.mono_state.type_subst);
        let saved_cfn = std::mem::take(&mut self.closure_state.closure_fn_types);
        // B-2026-07-15-8: a closure-VALUED free var (`let base = |x| x + 1;
        // let composed = |x| base(x) * 10;`) is captured into the env by value
        // (its `{fn_ptr, env_ptr}` fat pointer) and re-registered in
        // `self.variables` by the capture-load below. But the `take` above just
        // emptied `closure_fn_types`, so a call `base(x)` inside the body would
        // miss the `closure_fn_types.contains_key` dispatch in `compile_call`
        // and fall through to the unknown-callee const-0 stub — a SILENT wrong
        // result (codegen returned 0 where the interpreter computed the real
        // value). Re-register each captured free var's outer closure-value
        // `FunctionType` into the fresh body-scope map so the indirect-call
        // path (`compile_closure_call` → `load_variable` + `build_indirect_call`)
        // fires. Both unpack paths (plain `free_vars` and the path-precise
        // layout) register the root name in `self.variables`, so the
        // `load_variable` in `compile_closure_call` resolves either way.
        for name in &free_vars {
            if let Some(ft) = saved_cfn.get(name).copied() {
                self.closure_state.closure_fn_types.insert(name.clone(), ft);
            }
        }
        let saved_pct = self.closure_state.pending_closure_fn_type.take();
        // Isolate the f-string accumulator staging slot. A closure body
        // may stage an `fstr.acc` alloca (e.g. an f-string moved into
        // `Result.Err(f"…")`); that alloca lives in the *closure* fn, so
        // if `last_fstr_acc` leaks back to the outer scope the outer
        // function's cleanup path emits GEPs into it and LLVM rejects them
        // ("Instruction does not dominate all uses"). The closure owns its
        // own staged f-string (moved into the returned value or drained by
        // its scope cleanup), so the outer slot is saved and restored intact.
        let saved_fstr_acc = self.last_fstr_acc.take();
        // Isolate the scope-cleanup frame (mirrors `emit_par_branch_fn`).
        // The closure body's cleanup actions — e.g. an f-string
        // accumulator's free — must be registered AND emitted inside the
        // closure fn, where their allocas dominate, and drained before the
        // closure returns. Without isolation they land in the OUTER
        // function's frame and the outer drain emits GEPs into closure-fn
        // allocas, which LLVM rejects ("Instruction does not dominate all
        // uses" on `fstr.acc`). The body push below gives `track_*`/cleanup
        // registration a frame of its own.
        let saved_cleanup = std::mem::take(&mut self.drop_rc.scope_cleanup_actions);
        self.drop_rc.scope_cleanup_actions.push(Vec::new());
        // Isolate the par-branch cancel pointer (B-2026-06-18-10). When the
        // enclosing scope is a `par {}` branch the auto-par pass produced,
        // `branch_cancel_ptr` points at THAT branch fn's `cancel_flag` arg. The
        // closure is a SEPARATE function, so a method call in its body that runs
        // `emit_branch_cancel_check` would load the cancel flag from an argument
        // of the wrong function ("referring to an argument in another
        // function"). Clear it for the body and restore after, exactly as the
        // par/reduce/task-group emitters do at their function boundaries.
        let saved_cancel_ptr = self.conc.branch_cancel_ptr.take();
        // Isolate the owned-Vec/String PARAM set (B-2026-07-15-9). A closure's
        // owned heap params are caller-retained exactly like a function's: the
        // CALLER frees the arg buffer (materialized at the closure call site),
        // and the closure must deep-copy such a param if it flows to the return
        // (else the caller's arg-free and the result-binding's free double-free
        // the same buffer). Registering the closure's params here — with the
        // outer function's set swapped out and restored below — makes the
        // body's tail-return defensive copy (`maybe_defensive_copy_param_arg`)
        // and the capture defensive-copy (which key off `owned_vecstr_params`)
        // see the CLOSURE's params during the body compile, not the outer fn's.
        let saved_owned_vecstr_params = std::mem::take(&mut self.borrow_vars.owned_vecstr_params);
        // Isolate the borrowed-alias set (B-2026-07-18-42). A closure that
        // captures a whole heap Vec/String and RETURNS it (or otherwise
        // retaining-consumes it) must hand back an INDEPENDENT buffer, because in
        // BOTH env models the captured value's buffer is owned by something the
        // frame will still free:
        //   * stack-env (non-escaping): the env holds a bit-copy of the source's
        //     `{ptr,len,cap}` header — an ALIAS. The ownership pass treats the
        //     capture as a read/borrow, not a move (the source is still usable
        //     after the closure), so the enclosing frame stays the owner and
        //     frees the source at scope exit.
        //   * heap-env (escaping): the RC env box OWNS the captured buffer
        //     (deep-copied at capture for an owned param, moved-in with a
        //     cap-zeroed source for a local) and frees it via the per-closure
        //     env-drop fn at RC-zero. That env may be called any number of times,
        //     so the return cannot MOVE the buffer out of the env.
        // Either way, returning the captured value directly hands back an alias
        // of a buffer that gets freed elsewhere, so the receiver's free and that
        // owner's free hit the same buffer — a double-free under AOT/JIT (the
        // interpreter clones, so it was a run-vs-build divergence:
        // `fn f(x: String) -> String { let g = || x; g() }` and the escaping
        // `fn mk(x: String) -> Fn() -> String { || x }`). Registering each
        // captured heap var in `for_loop_borrow_vars` for the body compile routes
        // every retaining-consume site (tail return, tuple, push, struct field,
        // map value — all keyed on this set via `maybe_defensive_copy_param_arg`)
        // through a defensive deep-copy, so the returned value owns an
        // independent buffer while the env / source keeps its own. Taken +
        // restored so the outer fn / sibling closures don't inherit these names;
        // inserted at the capture-unpack step below.
        let saved_for_loop_borrow_vars = std::mem::take(&mut self.borrow_vars.for_loop_borrow_vars);
        // The borrow-mode registries for the closure's PARAMS (step 7b below
        // inserts a `ref T` / `mut ref T` param into both). Taken + restored
        // for the same reason as `variables` above: a param name is scoped to
        // this closure's body, and leaving its borrow mark behind makes the
        // OUTER function's later binding of that same name deref a value that
        // is not a pointer.
        //
        // B-2026-08-08-30. `Vec[i64].first().map(|x| x + 1)` followed by any
        // later `x` — the row's own `Some(x) =>` arm, or a plain `let x` —
        // read the stale mark: `load_variable` saw `ref_params["x"]` and
        // deref'd an `i64`, panicking `into_pointer_value`. The panic was the
        // LUCKY case. When the later `x` is a Vec, the bogus deref SUCCEEDS
        // and silently reads the wrong word: `let x: Vec[i64] = vec![1,2,3];
        // println(x.len())` printed 2 under `karac build` against the
        // interpreter's 3 — a run-vs-build divergence with no crash, which is
        // why this is restored wholesale rather than only where it panicked.
        // CLONED, not `take`n, unlike `variables` above: the body must still
        // see the ENCLOSING fn's borrow marks, because a captured `ref T`
        // keeps its name and `load_variable` has to keep deref'ing it inside
        // the closure. Taking would clear those for the body and silently
        // un-deref every captured borrow. Cloning changes nothing during the
        // body compile and drops only the param entries on the way out, which
        // is the whole defect.
        let saved_ref_params = self.borrow_vars.ref_params.clone();
        let saved_signature_ref_params = self.borrow_vars.signature_ref_params.clone();

        // 7. Build the closure body.
        self.current_fn = Some(closure_fn);
        let entry = self.context.append_basic_block(closure_fn, "entry");
        self.builder.position_at_end(entry);

        // 7a. Load captured vars from the env struct (param 0 = env ptr).
        let mut env_ptr = closure_fn.get_nth_param(0).unwrap().into_pointer_value();
        // Slice 1: a heap-env closure receives the RC box `{ refcount, env }` as
        // its env pointer; GEP past the refcount (field 1) to the env payload so
        // the unpack below is identical to the stack-env case.
        if is_heap_env {
            env_ptr = self
                .builder
                .build_struct_gep(env_box_ty, env_ptr, 2, "__env_payload")
                .unwrap();
        }
        // Load the env struct value through the env pointer.
        let env_val = self
            .builder
            .build_load::<BasicTypeEnum<'ctx>>(env_struct_ty.into(), env_ptr, "__env")
            .unwrap();

        // B-2026-07-18-46: captured whole heap-bearing STRUCT/ENUM vars (a
        // struct with a String/Vec field, etc.) — the Vec/String sibling of the
        // B-2026-07-18-42 borrow-alias marking. A struct capture is bit-copied
        // into the env, shallow-aliasing the source struct's field buffers
        // (which the frame's owner drop / the RC env-drop still frees), so a body
        // that RETURNS the captured struct must hand back an INDEPENDENT deep
        // clone. `for_loop_borrow_vars` (used for Vec/String) drives the flat
        // `emit_vecstr_defensive_copy`, which no-ops on a struct value, so this
        // needs the type-aware `emit_clone_fn_for_type_expr` instead — tracked
        // here and applied at the tail return below. (Records the capture's
        // concrete struct/enum type name.)
        let mut heap_struct_captures: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();

        if let Some(layout) = path_layout.as_ref() {
            // Per-path unpack: one env slot per captured CapturePath.
            // For whole-root entries the slot holds the root value as-is;
            // for path-precise entries we allocate a root-typed alloca
            // and stitch each leaf into its GEP chain, then register the
            // root alloca in `self.variables` so the body's `u.f` reads
            // walk it normally.
            let env_struct = env_val.into_struct_value();
            for (root_name, plan) in &layout.root_plans {
                if let Some(slot_idx) = plan.whole_root_slot {
                    let field_val = self
                        .builder
                        .build_extract_value(env_struct, slot_idx as u32, root_name)
                        .unwrap();
                    let alloca = self.create_entry_alloca(closure_fn, root_name, plan.root_ty);
                    self.builder.build_store(alloca, field_val).unwrap();
                    self.variables.insert(
                        root_name.clone(),
                        VarSlot {
                            ptr: alloca,
                            ty: plan.root_ty,
                        },
                    );
                } else {
                    // Stitch: allocate the root, write each captured leaf
                    // into its GEP chain. Other leaves stay undef — the
                    // ownership pass guarantees the body never reads them.
                    let alloca = self.create_entry_alloca(closure_fn, root_name, plan.root_ty);
                    for (slot_idx, gep_chain, leaf_ty) in &plan.sub_slots {
                        let leaf_val = self
                            .builder
                            .build_extract_value(
                                env_struct,
                                *slot_idx as u32,
                                &format!("{}.cap", root_name),
                            )
                            .unwrap();
                        let leaf_ptr = self.gep_root_chain(plan.root_ty, alloca, gep_chain);
                        self.builder.build_store(leaf_ptr, leaf_val).unwrap();
                        let _ = leaf_ty; // typed read at capture site; store inherits type from value.
                    }
                    self.variables.insert(
                        root_name.clone(),
                        VarSlot {
                            ptr: alloca,
                            ty: plan.root_ty,
                        },
                    );
                }
                if let Some(type_name) = &plan.type_name {
                    self.var_types
                        .var_type_names
                        .insert(root_name.clone(), type_name.clone());
                }
                // B-2026-07-18-42: a stack-env whole-heap capture aliases the
                // outer owner — mark it a borrowed alias so a body return / other
                // retaining consume deep-copies instead of handing back the alias.
                // Gated on `vec_elem_types` (the Vec/String set — matches
                // `maybe_defensive_copy_param_arg`'s own gate); a disjoint
                // sub-field/struct capture is naturally excluded.
                if !is_heap_env && self.var_types.vec_elem_types.contains_key(root_name) {
                    self.borrow_vars
                        .for_loop_borrow_vars
                        .insert(root_name.clone());
                }
                // B-2026-07-18-46: a whole heap-bearing STRUCT/ENUM capture (not
                // a Vec/String — those took the borrow-alias path above) records
                // its concrete type for the type-aware deep clone at the tail.
                if let Some((n, tn)) = self.captured_heap_agg_type(root_name) {
                    heap_struct_captures.insert(n, tn);
                }
            }
        } else if !free_vars.is_empty() {
            for (i, var_name) in free_vars.iter().enumerate() {
                let cap_ty = env_field_types[i];
                let field_val = self
                    .builder
                    .build_extract_value(env_val.into_struct_value(), i as u32, var_name)
                    .unwrap();
                if mutref_caps.contains(var_name) {
                    // By-reference (`mut ref`) capture: the env slot holds a
                    // POINTER to the outer binding. Register the var to read /
                    // write THROUGH it — no local copy — so body mutations land
                    // on the real outer slot (design.md Rule 2, B-2026-07-11-23).
                    self.variables.insert(
                        var_name.clone(),
                        VarSlot {
                            ptr: field_val.into_pointer_value(),
                            ty: orig_cap_tys[i],
                        },
                    );
                    if let Some(type_name) = saved_var_types.get(var_name) {
                        self.var_types
                            .var_type_names
                            .insert(var_name.clone(), type_name.clone());
                    }
                    continue;
                }
                let alloca = self.create_entry_alloca(closure_fn, var_name, cap_ty);
                self.builder.build_store(alloca, field_val).unwrap();
                self.variables.insert(
                    var_name.clone(),
                    VarSlot {
                        ptr: alloca,
                        ty: cap_ty,
                    },
                );
                // Propagate the outer scope's struct/enum type binding so
                // method dispatch inside the closure can route through the
                // user impl-block path.
                if let Some(type_name) = saved_var_types.get(var_name) {
                    self.var_types
                        .var_type_names
                        .insert(var_name.clone(), type_name.clone());
                }
                // B-2026-07-18-42: a whole-heap capture must be deep-copied at a
                // body return / other retaining consume — the buffer is owned by
                // the frame (stack-env alias) or the RC env box (heap-env), never
                // by the return value. Mark it borrowed so the consume sites
                // deep-copy. (mutref captures already `continue`d above.)
                if self.var_types.vec_elem_types.contains_key(var_name) {
                    self.borrow_vars
                        .for_loop_borrow_vars
                        .insert(var_name.clone());
                }
                // B-2026-07-18-46: a whole heap-bearing STRUCT/ENUM capture — the
                // aggregate sibling of the Vec/String borrow-alias above.
                if let Some((n, tn)) = self.captured_heap_agg_type(var_name) {
                    heap_struct_captures.insert(n, tn);
                }
            }
        }

        // 7b. Bind closure params (fn params 1..n).
        for (i, (cp, ty)) in params.iter().zip(param_llvm_types.iter()).enumerate() {
            let param_val = closure_fn.get_nth_param((i + 1) as u32).unwrap();
            let param_name = match &cp.pattern.kind {
                PatternKind::Binding(n) => n.clone(),
                _ => format!("_cp{}", i),
            };
            let alloca = self.create_entry_alloca(closure_fn, &param_name, *ty);
            self.builder.build_store(alloca, param_val).unwrap();
            self.variables.insert(
                param_name.clone(),
                VarSlot {
                    ptr: alloca,
                    ty: *ty,
                },
            );
            // Register the param's Kāra struct/enum name under
            // `var_type_names` so `compile_field_access` inside the body
            // can resolve `param.field` reads. Without this, a closure
            // body like `|s: Score| s.v` silently lowers `s.v` to the
            // `i64 0` placeholder (`field_index_for` returns None →
            // generic field-access fall-through). The inline thunk in
            // vec_method::emit_sort_by_key_inline_thunk works around it
            // with its own var_type_names insertion at the param-bind
            // step; precompiled-closure callees (sort_by_key with a
            // closure-typed local) had no equivalent and silently produced
            // a no-op comparator. Pull the type name from the param's
            // declared type expr (single-segment Path catches the common
            // shapes: `Score`, `Item`, etc.; tuple / generic / etc. fall
            // through and the body's field access just uses the existing
            // body-path lookups).
            if let Some(te) = cp.ty.as_ref() {
                if let TypeKind::Path(p) = &te.kind {
                    if let Some(seg) = p.segments.last() {
                        self.record_var_type_name(param_name.clone(), seg.clone());
                    }
                }
            }
            // B-2026-07-02-12: register the collection / String side-tables
            // for the param from its effective type — the annotation when
            // present, else the typechecker's inferred `Fn(...)` param type
            // at the closure span (same source as the LLVM types above).
            // Without this an un-annotated String param was invisible to
            // `string_vars`, so an f-string interpolation in the body
            // formatted the `{ptr,len,cap}` value's first word as an i64.
            let effective_te = cp
                .ty
                .as_ref()
                .or_else(|| inferred_param_tes.get(i).and_then(Option::as_ref));
            if let Some(te) = effective_te {
                let te = te.clone();
                // A `ref T` / `mut ref T` closure param's slot holds a POINTER,
                // exactly like the function-param path in `functions.rs` —
                // record the borrow so `load_variable` / `get_data_ptr` deref
                // it, and register the side tables from the BORROWED-OF type.
                // `register_var_from_type_expr` has no `Ref` arm, so handing it
                // the borrow type registers NOTHING: B-2026-08-05-15 leg 2, a
                // `|w: ref Vec[u8], i| w[i]` body failed to build with "Index
                // operator applied to non-array type" because `w` never reached
                // `vec_elem_types`, while `karac check` passed and the
                // interpreter ran it.
                // B-2026-08-15-28 — but NOT for a handle-backed builtin. A
                // `Map`/`Set`/`Tensor`/… value IS a pointer, and the closure ABI
                // passes that pointer directly (`call %closure_fn(%env, %m)`
                // where `%m` is the loaded handle). Registering it as a borrow
                // adds a second load on top of it, so the body derefs the handle
                // and calls the runtime with the map's first WORD as a pointer:
                // `|m| m.len()` returned 529 for a one-entry map, and
                // `m.contains_key(k)` segfaulted. The handle already IS the
                // reference — there is nothing to deref.
                let borrows_a_handle = match &te.kind {
                    TypeKind::Ref(inner) | TypeKind::MutRef(inner) => match &inner.kind {
                        TypeKind::Path(p) => p
                            .segments
                            .last()
                            .is_some_and(|n| super::types_lowering::builtin_opaque_ptr_handle(n)),
                        _ => false,
                    },
                    _ => false,
                };
                if let Some(inner_ty) = self.inner_type_of_ref(&te) {
                    if !borrows_a_handle {
                        self.borrow_vars
                            .ref_params
                            .insert(param_name.clone(), inner_ty);
                        self.borrow_vars
                            .signature_ref_params
                            .insert(param_name.clone());
                    }
                }
                match &te.kind {
                    TypeKind::Ref(inner) | TypeKind::MutRef(inner) => {
                        let inner = (**inner).clone();
                        self.register_var_from_type_expr(&param_name, &inner);
                    }
                    _ => self.register_var_from_type_expr(&param_name, &te),
                }
            }
            // Owned (non-`ref`) Vec/String param → the caller-retained set, so a
            // tail return of this param deep-copies (B-2026-07-15-9). Mirrors the
            // function-param registration (`functions.rs`): gated on a heap
            // element type (`vec_elem_types` populated by the
            // `register_var_from_type_expr` above) and not a borrow mode.
            let is_borrow_param = matches!(
                cp.ty.as_ref().map(|t| &t.kind),
                Some(TypeKind::Ref(_) | TypeKind::MutRef(_) | TypeKind::MutSlice(_))
            );
            if !is_borrow_param && self.var_types.vec_elem_types.contains_key(&param_name) {
                self.borrow_vars
                    .owned_vecstr_params
                    .insert(param_name.clone());
            }
        }

        // 7b½. Currying (B-2026-07-12-12): if this closure's tail is itself a
        // capturing closure literal, mark that inner span heap-env for the
        // body compile so the nested `compile_closure` gives it a per-call RC
        // heap box (each `make(n)` instance owns a distinct env — no aliasing).
        // Saved/restored around the body so sibling closures don't inherit it.
        let saved_heap_spans = self.fn_ctx.current_fn_heap_closure_spans.clone();
        if let Some(inner_span) = self.closure_tail_heap_closure_span(params, body) {
            self.fn_ctx.current_fn_heap_closure_spans.insert(inner_span);
        }

        // 7c. Compile body and build return.
        //
        // A BLOCK body is compiled like a function body — via the raw
        // `compile_block` (stmts + tail), NOT `compile_expr` (which routes
        // a block through `compile_block_with_frame`, opening a *nested*
        // scope whose cleanup runs INSIDE the body compilation). With the
        // nested scope, a returned heap binding `|| { let s = mk(); s }`
        // is freed by the block's scope-exit cleanup *before* the
        // tail-return suppression below can zero its `cap`, so the closure
        // hands back a dangling pointer (use-after-free / double-free).
        // Compiling the block raw makes its statements register their
        // cleanups in THIS closure's already-pushed frame, drained after
        // suppression. Non-block bodies are single expressions.
        let mut result = match &body.kind {
            ExprKind::Block(block) | ExprKind::Seq(block) => self
                .compile_block(block)?
                .unwrap_or_else(|| self.context.i64_type().const_int(0, false).into()),
            _ => self.compile_expr(body)?,
        };
        if self
            .builder
            .get_insert_block()
            .unwrap()
            .get_terminator()
            .is_none()
        {
            // Move-aware drain (mirrors `compile_function`): suppress the
            // cleanup of whatever the closure RETURNS so the frame drain
            // below doesn't free the value handed back to the caller. For a
            // block body, suppress the tail binding's cleanup
            // (Vec/String/Map/user-`Drop`). For ANY returned f-string
            // (`|| f"…"`, or a block tail `f"…"`, or one moved into
            // `Result.Err(f"…")`), zero its accumulator `cap` so the
            // drained free is a runtime no-op.
            let returned_tail: Option<&Expr> = match &body.kind {
                ExprKind::Block(block) | ExprKind::Seq(block) => {
                    self.suppress_cleanup_for_tail_return(block);
                    block.final_expr.as_deref()
                }
                _ => Some(body),
            };
            if let Some(t) = returned_tail {
                self.suppress_fstr_acc_if_moved_out(t);
                // Owned Vec/String PARAM in tail position (`|s| s`, `|s| { s }`,
                // or buried in a branch tail): the caller now frees the arg
                // buffer it passed (materialized at the closure call site,
                // B-2026-07-15-9), and the consumer of THIS return also frees
                // the value — so hand back a deep copy, exactly as
                // `compile_function`'s tail does. A no-op unless `t` ultimately
                // names one of this closure's `owned_vecstr_params`, so the
                // common fresh-result tail (`|s| wrap(wrap(s))`) is untouched.
                // A closure tail is RETURNED, so its value is always owned.
                result = self.deepcopy_owned_param_branch_tail(t, result, true)?;
                // B-2026-07-18-46: a captured heap-bearing STRUCT/ENUM returned
                // whole (`|| w`) — the aggregate sibling of the Vec/String
                // deep-copy above. The bit-copied capture shallow-aliases the
                // source struct's field buffers (freed by the frame's owner drop
                // / the RC env-drop), so hand back a type-aware deep clone. A
                // no-op unless the tail names one of this closure's tracked
                // heap-struct captures. Recurses through block tails like the
                // Vec/String helper.
                result = self.deepcopy_captured_heap_agg_tail(t, result, &heap_struct_captures);
            }
            // Drain the closure's own cleanup frame before returning, so its
            // f-string / heap-local cleanups are emitted in THIS fn
            // (alloca-dominated) rather than leaking into the outer frame.
            self.emit_scope_cleanup();
            self.builder.build_return(Some(&result)).unwrap();
        }
        // Restore the heap-span set (7b½): the inner-closure marking is scoped
        // to this closure's body compile only.
        self.fn_ctx.current_fn_heap_closure_spans = saved_heap_spans;

        // 8. Restore outer state.
        self.mono_state.type_subst = saved_subst;
        self.fn_ctx.loop_stack = saved_loop_stack;
        self.var_types.var_type_names = saved_var_types;
        self.variables = saved_vars;
        self.current_fn = saved_fn;
        self.closure_state.closure_fn_types = saved_cfn;
        self.closure_state.pending_closure_fn_type = saved_pct;
        self.last_fstr_acc = saved_fstr_acc;
        self.drop_rc.scope_cleanup_actions = saved_cleanup;
        self.conc.branch_cancel_ptr = saved_cancel_ptr;
        // Restore the outer fn's owned-param set BEFORE the env is built below
        // (the capture defensive-copy at env-build time keys off the OUTER fn's
        // `owned_vecstr_params` to decide whether a captured Vec/String is a
        // caller-retained param — B-2026-06-22-2).
        self.borrow_vars.owned_vecstr_params = saved_owned_vecstr_params;
        // Restore the borrowed-alias set (B-2026-07-18-42): the captured-heap
        // borrow marks are scoped to this closure's body compile only, so the
        // outer fn / sibling closures don't inherit them. Restored BEFORE the
        // env is built below (env-build reads `owned_vecstr_params`, not this
        // set, but keep the restore grouped with the other body-scoped state).
        self.borrow_vars.for_loop_borrow_vars = saved_for_loop_borrow_vars;
        // Drop this closure's param borrow marks (B-2026-08-08-30). Grouped
        // with the other body-scoped restores above; see the save site for why
        // leaving them behind miscompiles the outer fn's next same-named
        // binding rather than merely panicking.
        self.borrow_vars.ref_params = saved_ref_params;
        self.borrow_vars.signature_ref_params = saved_signature_ref_params;
        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }

        // 9. In the outer context, allocate and populate the env struct.
        //    Non-escaping closure → cheap stack alloca (freed with the frame).
        //    Escaping (heap) closure → reference-counted heap box
        //    `{ refcount=1, env_struct }`; the env captures are stored into the
        //    box payload, the fat pointer carries the BOX pointer, and the
        //    owning caller binding frees it via `FreeClosureEnv` (Slice 1).
        let outer_fn = self.current_fn.unwrap();
        let (env_alloca, fat_env_ptr) = if is_heap_env {
            let box_ptr = self.emit_rc_alloc(env_box_ty);
            // Slice 2: store the per-closure env-drop fn (frees captured
            // String/Vec buffers before the box is freed) at box field 1, or a
            // null pointer when the env is POD-only (Slice 1).
            let drop_ptr_ty = self.context.ptr_type(AddressSpace::default());
            let drop_val: BasicValueEnum<'ctx> =
                match self.synthesize_closure_env_drop_fn(id, env_box_ty, env_struct_ty) {
                    Some(f) => f.as_global_value().as_pointer_value().into(),
                    None => drop_ptr_ty.const_null().into(),
                };
            let drop_slot = self
                .builder
                .build_struct_gep(env_box_ty, box_ptr, 1, "__env_drop_slot")
                .unwrap();
            self.builder.build_store(drop_slot, drop_val).unwrap();
            let payload = self
                .builder
                .build_struct_gep(env_box_ty, box_ptr, 2, "__env_box_payload")
                .unwrap();
            (payload, box_ptr)
        } else {
            let a = self.create_entry_alloca(outer_fn, "__closure_env", env_struct_ty.into());
            (a, a)
        };
        if let Some(layout) = path_layout.as_ref() {
            // Per-path capture: for each env slot, walk the source root's
            // GEP chain and store the leaf value into the slot.
            if !layout.slot_sources.is_empty() {
                let mut env_agg = env_struct_ty.get_undef();
                for (i, (root, gep_chain)) in layout.slot_sources.iter().enumerate() {
                    let slot = self.variables[root];
                    let val = if gep_chain.is_empty() {
                        // Whole-root: load the root binding directly.
                        self.builder.build_load(slot.ty, slot.ptr, root).unwrap()
                    } else {
                        // Path-precise: GEP into the root's alloca, load
                        // the leaf. `slot.ptr` is the alloca holding the
                        // root struct value (root captures gated to
                        // non-RC, non-ref-param roots in
                        // `build_capture_path_layout` so this is always
                        // a direct struct alloca).
                        let leaf_ptr = self.gep_root_chain(slot.ty, slot.ptr, gep_chain);
                        let leaf_ty = self.leaf_type_for_chain(slot.ty, gep_chain);
                        self.builder
                            .build_load(leaf_ty, leaf_ptr, &format!("{}.cap.read", root))
                            .unwrap()
                    };
                    env_agg = self
                        .builder
                        .build_insert_value(env_agg, val, i as u32, "__env_field")
                        .unwrap()
                        .into_struct_value();
                }
                self.builder.build_store(env_alloca, env_agg).unwrap();
            }
        } else if !free_vars.is_empty() {
            // Build the env struct by inserting each captured value. A
            // `mut ref`-captured name stores the ADDRESS of its outer slot (by-
            // reference capture, 1a) so the closure body writes through to the
            // real binding; every other name stores its loaded value.
            let mut env_agg = env_struct_ty.get_undef();
            for (i, var_name) in free_vars.iter().enumerate() {
                let slot = self.variables[var_name];
                let mut val: BasicValueEnum<'ctx> = if mutref_caps.contains(var_name) {
                    slot.ptr.into()
                } else {
                    self.builder
                        .build_load(slot.ty, slot.ptr, var_name)
                        .unwrap()
                };
                // Slice 2 (B-2026-06-22-2): a captured owned Vec/String PARAMETER
                // is a by-value {ptr,len,cap} that SHALLOW-ALIASES the caller's
                // buffer (the caller passes by value and RETAINS its own drop, per
                // the `owned_vecstr_params` convention — codegen does not force a
                // caller-side move). Moving it into the env would then double-free:
                // the caller frees its buffer AND the env-drop frees the same one.
                // DEEP-COPY it into the env instead — the env owns an INDEPENDENT
                // buffer, the caller keeps its own. A captured LOCAL is genuinely
                // this-frame-owned (absent from `owned_vecstr_params`), so it keeps
                // the cheap MOVE (alias + the cap-zero below). Shallow outer-buffer
                // copy — matches the env-drop's outer-only free; the heap-capture
                // gate keeps nested `Vec[heap]` elements out, so no element aliasing.
                if is_heap_env
                    && self.borrow_vars.owned_vecstr_params.contains(var_name)
                    && matches!(slot.ty, BasicTypeEnum::StructType(held) if held == self.vec_struct_type())
                {
                    if let Some(elem_ty) = self.var_types.vec_elem_types.get(var_name).copied() {
                        val = self.emit_vecstr_defensive_copy(val, elem_ty, None);
                    }
                }
                env_agg = self
                    .builder
                    .build_insert_value(env_agg, val, i as u32, "__env_field")
                    .unwrap()
                    .into_struct_value();
                // Slice 2 move-out (B-2026-06-22-2): the heap env now OWNS this
                // captured String/Vec buffer (freed by the env-drop fn at
                // RC-zero). Zero the SOURCE binding's `cap` so its own scope-exit
                // `FreeVecBuffer` `cap > 0` guard skips — the env is the sole
                // owner, no double-free. Only for a heap-env (escaping) closure:
                // a stack-env closure aliases a still-live source that keeps
                // ownership and must NOT be suppressed. The `slot.ty == vec_ty`
                // check is the correct guard: it holds precisely for a Vec/String
                // value held INLINE (24-byte `{ptr,len,cap}`) — a `ref` slot is
                // an 8-byte pointer, excluded (and a by-ref capture never reaches
                // here anyway, its env field being a pointer the gate rejects).
                // Gating on `vec_elem_types` instead would MISS a Vec *param*
                // (absent from that map) and double-free.
                if is_heap_env {
                    let vec_ty = self.vec_struct_type();
                    if matches!(slot.ty, BasicTypeEnum::StructType(held) if held == vec_ty) {
                        if let Ok(cap_ptr) =
                            self.builder
                                .build_struct_gep(vec_ty, slot.ptr, 2, "clo.cap.move")
                        {
                            let _ = self
                                .builder
                                .build_store(cap_ptr, self.context.i64_type().const_int(0, false));
                        }
                    }
                }
            }
            self.builder.build_store(env_alloca, env_agg).unwrap();
        }

        // 10. Build the fat-pointer closure struct: { fn_ptr, env_alloca }.
        let fn_ptr = closure_fn.as_global_value().as_pointer_value();
        let fat_ptr_ty = self.closure_value_type();
        let mut fat = fat_ptr_ty.get_undef();
        fat = self
            .builder
            .build_insert_value(fat, fn_ptr, 0, "closure_fn")
            .unwrap()
            .into_struct_value();
        fat = self
            .builder
            .build_insert_value(fat, fat_env_ptr, 1, "closure_env")
            .unwrap()
            .into_struct_value();

        // 11. Stage the LLVM function type for the surrounding let binding.
        self.closure_state.pending_closure_fn_type = Some(fn_type);

        Ok(fat.into())
    }

    /// B-2026-07-18-46: classify a closure capture as a whole heap-bearing
    /// STRUCT/ENUM (a value struct/enum that owns a String/Vec below it), which
    /// must be deep-cloned when returned from the body. Returns
    /// `Some((name, type_name))` for such a capture; `None` for a Vec/String
    /// capture (handled by the `for_loop_borrow_vars` flat-copy path,
    /// B-2026-07-18-42), a shared (RC) aggregate (refcount machinery), a POD
    /// aggregate (nothing to deep-copy), or an unknown/unnamed capture.
    fn captured_heap_agg_type(&self, name: &str) -> Option<(String, String)> {
        // Vec/String captures take the flat borrow-alias path.
        if self.var_types.vec_elem_types.contains_key(name) {
            return None;
        }
        let tn = self.var_types.var_type_names.get(name)?.clone();
        // Shared (RC) aggregates are co-owned via refcount, not deep-cloned.
        if self.type_decls.shared_types.contains_key(&tn) {
            return None;
        }
        if !(self.type_decls.struct_types.contains_key(&tn)
            || self.type_decls.enum_layouts.contains_key(&tn))
        {
            return None;
        }
        let te = TypeExpr {
            kind: TypeKind::Path(PathExpr {
                segments: vec![tn.clone()],
                generic_args: None,
                span: Span::default(),
            }),
            span: Span::default(),
        };
        // Only a heap-BEARING aggregate needs the clone; a POD struct/enum is a
        // bit-copy either way.
        if !self.type_expr_has_drop_heap(&te) {
            return None;
        }
        Some((name.to_string(), tn))
    }

    /// B-2026-07-18-46: deep-clone a captured heap-bearing struct/enum returned
    /// as a closure body's tail (`|| w` / `|| { …; w }`). The env's bit-copy
    /// shallow-aliases the source struct's field buffers, which the frame's owner
    /// drop (stack-env) or the RC env-drop (heap-env) still frees — so the
    /// returned value must own INDEPENDENT buffers. Emits a type-aware
    /// `karac_clone_<T>` (the `#[derive(Clone)]` analog the struct/enum drop
    /// mirrors). Recurses through block/unsafe tails to the leaf identifier, like
    /// `deepcopy_owned_param_branch_tail`. No-op unless the tail leaf is a
    /// tracked heap-struct capture.
    fn deepcopy_captured_heap_agg_tail(
        &mut self,
        tail: &Expr,
        val: BasicValueEnum<'ctx>,
        captures: &HashMap<String, String>,
    ) -> BasicValueEnum<'ctx> {
        let tn = match &tail.kind {
            ExprKind::Identifier(n) => match captures.get(n) {
                Some(t) => t.clone(),
                None => return val,
            },
            ExprKind::Block(b) | ExprKind::Seq(b) | ExprKind::Unsafe(b) => {
                return match b.final_expr.as_deref() {
                    Some(inner) => self.deepcopy_captured_heap_agg_tail(inner, val, captures),
                    None => val,
                };
            }
            _ => return val,
        };
        let Some(fn_val) = self.current_fn else {
            return val;
        };
        let te = TypeExpr {
            kind: TypeKind::Path(PathExpr {
                segments: vec![tn],
                generic_args: None,
                span: Span::default(),
            }),
            span: Span::default(),
        };
        let clone_fn = self.emit_clone_fn_for_type_expr(&te);
        // `emit_clone_fn_*` / `create_entry_alloca` may move the builder —
        // re-anchor to the tail block before emitting the copy.
        let cur = self.builder.get_insert_block();
        let val_ty = val.get_type();
        let src = self.create_entry_alloca(fn_val, "cap.agg.clone.src", val_ty);
        let dst = self.create_entry_alloca(fn_val, "cap.agg.clone.dst", val_ty);
        if let Some(bb) = cur {
            self.builder.position_at_end(bb);
        }
        self.builder.build_store(src, val).unwrap();
        self.builder
            .build_call(clone_fn, &[src.into(), dst.into()], "cap.agg.clone")
            .unwrap();
        self.builder
            .build_load(val_ty, dst, "cap.agg.cloned")
            .unwrap()
    }

    /// Slice 2 (B-2026-06-22-2): synthesize the per-closure env-drop fn that
    /// frees a heap-env closure's captured String/Vec buffers before the RC box
    /// is reclaimed. Emitted as `__karac_closure_env_drop_<id>(ptr box)`: it
    /// GEPs to the env payload (box field 2 — the box is
    /// `{ i64 rc, ptr drop, env }`) and runs `emit_aggregate_heap_field_frees`
    /// over the env struct, so each captured String/Vec field's buffer is
    /// `cap`-guarded-freed (a moved-in capture carries a live `cap`; a POD-only
    /// env owns no heap → `None`, and the box then stores a null drop slot).
    /// Called from the free path of `emit_heap_closure_env_dec`. Not memoized —
    /// a closure literal compiles once, so each heap-capturing closure gets its
    /// own drop fn keyed by the unique closure `id`.
    pub(super) fn synthesize_closure_env_drop_fn(
        &mut self,
        id: u32,
        env_box_ty: StructType<'ctx>,
        env_struct_ty: StructType<'ctx>,
    ) -> Option<FunctionValue<'ctx>> {
        if !self.aggregate_has_heap_field(env_struct_ty) {
            return None;
        }
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let saved_bb = self.builder.get_insert_block();
        let saved_fn = self.current_fn;
        let fn_name = format!("__karac_closure_env_drop_{}", id);
        let drop_fn_ty = self.context.void_type().fn_type(&[ptr_ty.into()], false);
        let drop_fn = self.module.add_function(
            &fn_name,
            drop_fn_ty,
            Some(inkwell::module::Linkage::Internal),
        );
        // `emit_aggregate_heap_field_frees` appends BBs to `current_fn` — point
        // it at the drop fn during synthesis, then restore the outer context.
        self.current_fn = Some(drop_fn);
        let entry = self.context.append_basic_block(drop_fn, "entry");
        self.builder.position_at_end(entry);
        let box_ptr = drop_fn.get_nth_param(0).unwrap().into_pointer_value();
        let payload = self
            .builder
            .build_struct_gep(env_box_ty, box_ptr, 2, "env.payload")
            .unwrap();
        self.emit_aggregate_heap_field_frees(payload, env_struct_ty);
        self.builder.build_return(None).unwrap();
        self.current_fn = saved_fn;
        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        Some(drop_fn)
    }

    /// Execute an indirect call through a closure fat-pointer variable.
    pub(super) fn compile_closure_call(
        &mut self,
        name: &str,
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let fn_type = match self.closure_state.closure_fn_types.get(name).copied() {
            Some(t) => t,
            None => return Ok(self.context.i64_type().const_int(0, false).into()),
        };

        // Load the closure fat pointer value { fn_ptr, env_ptr }.
        let fat_val = self.load_variable(name)?;
        let fat_sv = fat_val.into_struct_value();
        let fn_ptr = self
            .builder
            .build_extract_value(fat_sv, 0, "closure_fn")
            .unwrap()
            .into_pointer_value();
        let env_ptr = self
            .builder
            .build_extract_value(fat_sv, 1, "closure_env")
            .unwrap()
            .into_pointer_value();

        // Build call args: env_ptr first, then user-supplied args, each
        // marshalled to the signature's declared param type (B-2026-08-05-15).
        let declared = fn_type.get_param_types();
        let mut call_args: Vec<BasicMetadataValueEnum<'ctx>> =
            vec![BasicMetadataValueEnum::from(env_ptr)];
        for (i, arg) in args.iter().enumerate() {
            // `declared[0]` is the env pointer, so user arg `i` is `declared[i + 1]`.
            let val = self.compile_indirect_call_arg(arg, i, declared.get(i + 1).copied())?;
            call_args.push(BasicMetadataValueEnum::from(val));
        }

        let call = self
            .builder
            .build_indirect_call(fn_type, fn_ptr, &call_args, "closure_call")
            .unwrap();

        let basic_val = call.try_as_basic_value();
        if basic_val.is_instruction() {
            Ok(self.context.i64_type().const_int(0, false).into())
        } else {
            Ok(basic_val.unwrap_basic())
        }
    }

    /// Marshal one argument for an indirect (closure-ABI) call so it matches
    /// `declared`, the callee signature's own param type at that position.
    ///
    /// B-2026-08-05-15: this used to compile every argument by value, which is
    /// only right for the params whose direct-call ABI is by-value. A `ref T` /
    /// `mut ref T` param lowers to `ptr` (`llvm_type_for_type_expr`'s
    /// `TypeKind::Ref | MutRef` arm), and both indirect signatures agree with
    /// that — `env_first_fn_type` copies the target's own param types, and
    /// `closure_abi_fn_type` lowers the `Fn(..)` annotation through the same
    /// function. So the SIGNATURES were always right and only the call site was
    /// wrong: it pushed a `{ptr, i64, i64}` Vec triple into a `ptr` slot, which
    /// LLVM's verifier rejects outright, so `let f = takes_ref_vec; f(v, i)`
    /// would not build while `karac check` passed and the interpreter ran it.
    ///
    /// The by-pointer branch mirrors the direct-call path in `call_dispatch.rs`
    /// exactly: the address of a named binding's storage, an element borrow for
    /// an index expression, or a materialized temp for any other rvalue.
    ///
    /// Deciding by the DECLARED type rather than by a semantic ref-flag is what
    /// keeps this correct for a `shared` handle, whose param also lowers to
    /// `ptr` but whose argument is *already* a pointer value: for those,
    /// `ident_arg_needs_address` is false and the pointer passes straight
    /// through, so the address of the slot (a pointer-to-pointer) is never
    /// taken.
    fn compile_indirect_call_arg(
        &mut self,
        arg: &CallArg,
        idx: usize,
        declared: Option<BasicMetadataTypeEnum<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        if !matches!(declared, Some(BasicMetadataTypeEnum::PointerType(_))) {
            let val = self.compile_expr(&arg.value)?;
            self.free_fresh_owned_heap_closure_arg(&arg.value, val);
            return Ok(val);
        }
        if let ExprKind::Identifier(name) = &arg.value.kind {
            if self.ident_arg_needs_address(name) {
                if let Some(ptr) = self.get_data_ptr(name) {
                    return Ok(ptr.into());
                }
            }
            let val = self.compile_expr(&arg.value)?;
            self.free_fresh_owned_heap_closure_arg(&arg.value, val);
            return Ok(val);
        }
        if let Some(elem_ptr) = self.ref_arg_index_borrow_ptr(&arg.value)? {
            return Ok(elem_ptr.into());
        }
        let val = self.compile_expr(&arg.value)?;
        if val.is_pointer_value() {
            self.free_fresh_owned_heap_closure_arg(&arg.value, val);
            return Ok(val);
        }
        Ok(self.materialize_rvalue_for_ref_arg(val, idx))
    }

    /// Whether a named binding passed to a by-pointer (`ref`/`mut ref`) param
    /// must be handed the ADDRESS of its storage rather than its loaded value.
    ///
    /// True for a binding that stores its value inline (a `Vec`/`String`/struct
    /// alloca), and for the two indirections `get_data_ptr` already unwraps: a
    /// `ref` param forwarding its own borrow, and an RC-promoted binding whose
    /// data sits past the refcount header. False for a binding that already
    /// holds a plain pointer — a `shared` handle — where taking the slot's
    /// address would produce a pointer-to-pointer.
    fn ident_arg_needs_address(&self, name: &str) -> bool {
        if self.borrow_vars.ref_params.contains_key(name)
            || self.drop_rc.rc_fallback_heap_types.contains_key(name)
        {
            return true;
        }
        self.variables
            .get(name)
            .is_some_and(|slot| !slot.ty.is_pointer_type())
    }

    /// Caller-side cleanup of a FRESH owned heap arg (a `String.from(..)` /
    /// user-fn result / collection literal producing a `{ptr,len,cap}` temp)
    /// passed BY VALUE into a closure. A closure's owned Vec/String param is
    /// caller-retained (it deep-copies the param on any return), so the caller
    /// must free the temp — the exact `is_fresh_heap_call_arg` →
    /// `materialize_owned_temp` cleanup the direct-call path performs
    /// (`call_dispatch.rs`). Without it the temp leaks once per call
    /// (B-2026-07-15-9). A bound local / borrow / f-string-accumulator arg is
    /// excluded (its own binding owns the free / it stages its own cleanup), so
    /// no double-free with a caller-scope owner. Shared by the named-binding and
    /// value-callee closure-call paths.
    fn free_fresh_owned_heap_closure_arg(&mut self, arg: &Expr, val: BasicValueEnum<'ctx>) {
        if self.expr_yields_fresh_owned_temp(arg)
            && self.llvm_ty_is_vec_struct(val.get_type())
            && !self.rhs_stages_fstr_acc(arg)
        {
            self.materialize_owned_temp(val, (arg.span.offset, arg.span.length));
        }
    }

    /// Execute an indirect call through a closure fat-pointer VALUE produced by
    /// an arbitrary callee EXPRESSION rather than a named binding — a struct
    /// field `(h.f)(x)`, a Vec/array index `v[i](x)`, a tuple index `t.0(x)`, a
    /// parenthesized closure literal `(|x| x)(a)`, or a call result. The named-
    /// identifier closure case stays on the faster `closure_fn_types` /
    /// `load_variable` path in `compile_closure_call`; this generalization
    /// covers every other place expression that evaluates to a
    /// `{fn_ptr, env_ptr}` fat pointer (B-2026-06-22-4 — previously these fell
    /// through to a const-0 stub, a silent wrong-output miscompile under
    /// `karac build` while `karac run` was correct).
    ///
    /// The env-first ABI `FunctionType` is recovered from the callee
    /// expression's recorded `Fn(..)` type in `fn_value_typed_exprs` (the same
    /// lowering-pass span table `let_binding_fn_value_type` uses for an
    /// un-annotated `let g = h.f;`). Returns `Ok(None)` when the callee is not a
    /// function-typed expression, so the caller falls through to its existing
    /// unknown-callee const-0 fallback unchanged.
    pub(super) fn compile_closure_value_call(
        &mut self,
        callee: &Expr,
        args: &[CallArg],
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        // Recover the callee's `Fn(..)` signature from the inferred-type span
        // table; bail out (the caller keeps its const-0 fallback) when the
        // callee isn't a function value.
        let fn_type = match self
            .span_tables
            .fn_value_typed_exprs
            .get(&(callee.span.offset, callee.span.length))
            .map(|t| &t.kind)
        {
            Some(TypeKind::FnType {
                params,
                return_type,
                ..
            }) => self.closure_abi_fn_type(params, return_type.as_deref()),
            _ => return Ok(None),
        };

        // Evaluate the callee to its fat pointer { fn_ptr, env_ptr }.
        //
        // When the callee is a closure LITERAL, compiling it here EMITS the
        // function, and `compile_closure` publishes the signature it actually
        // used in `pending_closure_fn_type`. Prefer that over the surface
        // `Fn(..)` lowering above, which is only a prediction of it.
        //
        // The two disagree whenever the closure's declared return is a BORROW
        // that its body materializes by value. `Vec[String].first().map(|s| s)`
        // is recorded `Fn(ref String) -> ref String`, so this call site lowered
        // the return as one `ptr` word, while the emitted body deep-copies into
        // the owned `{ptr,len,cap}` aggregate and returns all three. Reading a
        // 3-word return through a 1-word signature dropped `len` and `cap`, and
        // the program printed the empty string against the interpreter's `ab`.
        // Nothing catches it: an indirect call's callee type is not verified.
        //
        // Its scalar sibling (`|x| x`, recorded `-> ref i64`) has the identical
        // disagreement and is correct only by luck — an `i64` read back through
        // a `ptr` return keeps its bits, so the payload word survives.
        // B-2026-08-09-5.
        //
        // Gated on the callee BEING a closure literal, not merely on the slot
        // being populated afterwards. Every other callee shape this function
        // serves (struct field, Vec/tuple index, call result) emits no closure
        // of its own, so the slot could only be filled by an unrelated literal
        // compiled while evaluating the callee — `pick(|x| x + 1)(5)` would hand
        // this call the ARGUMENT closure's signature. The slot is saved and
        // restored either way so an enclosing `let` still sees its own.
        let is_closure_literal = matches!(callee.kind, ExprKind::Closure { .. });
        let saved_pending = self.closure_state.pending_closure_fn_type.take();
        let fat_val = self.compile_expr(callee)?;
        let emitted = self.closure_state.pending_closure_fn_type.take();
        self.closure_state.pending_closure_fn_type = saved_pending;
        let fn_type = if is_closure_literal {
            emitted.unwrap_or(fn_type)
        } else {
            fn_type
        };
        let fat_sv = fat_val.into_struct_value();
        let fn_ptr = self
            .builder
            .build_extract_value(fat_sv, 0, "closure_fn")
            .unwrap()
            .into_pointer_value();
        let env_ptr = self
            .builder
            .build_extract_value(fat_sv, 1, "closure_env")
            .unwrap()
            .into_pointer_value();

        // Build call args: env_ptr first, then user-supplied args, each
        // marshalled to the signature's declared param type (B-2026-08-05-15).
        let declared = fn_type.get_param_types();
        let mut call_args: Vec<BasicMetadataValueEnum<'ctx>> =
            vec![BasicMetadataValueEnum::from(env_ptr)];
        for (i, arg) in args.iter().enumerate() {
            // `declared[0]` is the env pointer, so user arg `i` is `declared[i + 1]`.
            let val = self.compile_indirect_call_arg(arg, i, declared.get(i + 1).copied())?;
            call_args.push(BasicMetadataValueEnum::from(val));
        }

        let call = self
            .builder
            .build_indirect_call(fn_type, fn_ptr, &call_args, "closure_call")
            .unwrap();

        let basic_val = call.try_as_basic_value();
        if basic_val.is_instruction() {
            Ok(Some(self.context.i64_type().const_int(0, false).into()))
        } else {
            Ok(Some(basic_val.unwrap_basic()))
        }
    }

    /// Lightweight return-type inference for closure bodies.
    /// Walks the expression shallowly to determine the LLVM type without building IR.
    pub(super) fn infer_closure_return_type(
        &self,
        expr: &Expr,
        param_types: &HashMap<String, BasicTypeEnum<'ctx>>,
    ) -> BasicTypeEnum<'ctx> {
        match &expr.kind {
            ExprKind::Integer(_, sfx) => self.llvm_int_type_for_suffix(*sfx).into(),
            ExprKind::Float(_, sfx) => self.llvm_float_type_for_suffix(*sfx).into(),
            ExprKind::Bool(_) => self.context.bool_type().into(),
            ExprKind::CharLit(_) => self.context.i32_type().into(),
            ExprKind::ByteLit(_) | ExprKind::ByteStringLit(_) => self.context.i8_type().into(),
            // B-2026-08-08-22: the `{ptr, len, cap}` String aggregate, NOT a
            // bare `ptr` into rodata. This arm used to say `ptr` on the reading
            // that a literal is a BORROWED `str` — true of the literal, false
            // of what the closure body actually emits. Compiling `|x| "fixed"`
            // materializes an owned String constant and returns it by value
            // (`ret { ptr, i64, i64 } { ptr @str.1, i64 5, i64 0 }`), so the
            // `ptr` signature disagreed with the body's own `ret` and LLVM
            // module verification rejected the function. The sibling arm below
            // already had this right for f-strings; a bare literal reaches the
            // same place by the same coercion.
            //
            // Not `map`-specific despite the row that found it: `let f = |x:
            // i64| "fixed"; f(1)` fails identically with no combinator in
            // sight, and the same defect reached through a local
            // (`|x| { let t = "fixed"; t }`) fails too.
            ExprKind::StringLit(_) => self.vec_struct_type().into(),
            // An f-string evaluates to an owned `String` — the `{ptr, len,
            // cap}` heap-string aggregate (same representation as the literal
            // arm above).
            ExprKind::InterpolatedStringLit(_) => self.vec_struct_type().into(),
            ExprKind::Identifier(name) => {
                // B-2026-07-13-20 sibling: a bare `None` tail (`|n| if n>0 {
                // Some(..) } else { None }`) evaluates to the `Option` enum's
                // type-erased layout, NOT the `i64` fallback — else the closure
                // fn is declared `-> i64` while the body returns the 4-word Option
                // (LLVM verifier failure).
                if name == "None" {
                    if let Some(l) = self.type_decls.enum_layouts.get("Option") {
                        return l.llvm_type.into();
                    }
                }
                if let Some(&ty) = param_types.get(name) {
                    return ty;
                }
                // A CAPTURED borrow (an enclosing `ref T` param, or a
                // match-arm/`let` borrow shim) reaches the same trap as a
                // borrow param: its slot holds a pointer, but the body reads it
                // through `load_variable`, which derefs. Report the POINTEE, so
                // a tail of `|_| outer` declares `T` rather than `ptr`.
                // B-2026-08-08-30, capture leg — `param_types` above covers the
                // closure's OWN params, this covers everything it closes over.
                if let Some(&inner) = self.borrow_vars.ref_params.get(name.as_str()) {
                    return inner;
                }
                if let Some(slot) = self.variables.get(name.as_str()) {
                    return slot.ty;
                }
                self.context.i64_type().into()
            }
            ExprKind::Binary { op, left, right } => match op {
                BinOp::Eq
                | BinOp::NotEq
                | BinOp::Lt
                | BinOp::LtEq
                | BinOp::Gt
                | BinOp::GtEq
                | BinOp::And
                | BinOp::Or => self.context.bool_type().into(),
                _ => {
                    let lt = self.infer_closure_return_type(left, param_types);
                    let rt = self.infer_closure_return_type(right, param_types);
                    if lt.is_float_type() || rt.is_float_type() {
                        self.context.f64_type().into()
                    } else if lt == self.vec_struct_type().into()
                        || rt == self.vec_struct_type().into()
                    {
                        // B-2026-08-08-22: String concatenation yields an OWNED
                        // String, so the `{ptr,len,cap}` aggregate wins over a
                        // borrowed `ptr` on either side. Returning `lt`
                        // unconditionally was right only when the left operand
                        // already carried the owned type: `|s| s + "!"` with an
                        // UN-ANNOTATED `s` sees `param_types["s"]` as a bare
                        // `ptr`, so the signature said `ptr` while the body
                        // returned `%cat.cap`, the concatenated aggregate —
                        // `ret { ptr, i64, i64 } … ptr` at verification. The
                        // annotated spelling built precisely because the
                        // annotation put the owned type on the left.
                        self.vec_struct_type().into()
                    } else {
                        lt
                    }
                }
            },
            ExprKind::Unary { operand, .. } => self.infer_closure_return_type(operand, param_types),
            ExprKind::MethodCall { method, .. } if method == "cmp" => self
                .type_decls
                .enum_layouts
                .get("Ordering")
                .map(|l| BasicTypeEnum::StructType(l.llvm_type))
                .unwrap_or_else(|| {
                    self.context
                        .struct_type(&[self.context.i64_type().into()], false)
                        .into()
                }),
            // A general method-call tail whose result the lowering pass
            // recorded as an ENUM instantiation (`Option[V]` / `Result[T, E]` /
            // a user enum) — e.g. `|k, v| m.insert(k, v)` where
            // `Map.insert -> Option[V]`, or `|v| v.pop()`. The closure then
            // returns that enum's type-erased layout, NOT the `i64` fallback
            // below — which declared the closure fn `-> i64` against the 4-word
            // Option body and failed the LLVM verifier ("return type does not
            // match operand type of return inst", B-2026-07-15-17). Mirrors the
            // `Some`/`Ok`/`Err` constructor arms; keyed on the span-recorded
            // `enum_inst_type_exprs`. A non-enum method result (a scalar, or a
            // String/Vec — the latter a separate un-recorded shape) keeps the
            // `i64` default below, unchanged.
            ExprKind::MethodCall { .. } => {
                if let Some(te) = self.enum_inst_type_from_span(expr) {
                    if let TypeKind::Path(p) = &te.kind {
                        if let Some(name) = p.segments.last() {
                            if let Some(l) = self.type_decls.enum_layouts.get(name.as_str()) {
                                return l.llvm_type.into();
                            }
                        }
                    }
                }
                self.context.i64_type().into()
            }
            ExprKind::Cast { ty, .. } => self.llvm_type_for_type_expr(ty),
            ExprKind::Block(block) | ExprKind::Seq(block) => {
                if let Some(final_expr) = &block.final_expr {
                    // Block-local `let` bindings aren't in
                    // `param_types`/`self.variables` at inference time (the body
                    // hasn't been compiled), so extend the inference scope with
                    // them — prefer each let's type annotation, else infer from
                    // its value (seeing earlier lets). The tail then resolves a
                    // block-local whether it's a bare identifier (`… ; v`) OR a
                    // nested position such as a tuple element (`… ; (v, 100)` —
                    // B-2026-07-13-20 tuple sibling, where the old
                    // bare-identifier-only special case left `v` at the i64
                    // fallback and the closure fn was declared `{i64,i64}` against
                    // a `{Vec,i64}` body → LLVM verifier failure).
                    let mut extended = param_types.clone();
                    for stmt in &block.stmts {
                        if let StmtKind::Let {
                            pattern, ty, value, ..
                        } = &stmt.kind
                        {
                            if let PatternKind::Binding(n) = &pattern.kind {
                                let t = match ty {
                                    Some(te) => self.llvm_type_for_type_expr(te),
                                    None => self.infer_closure_return_type(value, &extended),
                                };
                                extended.insert(n.clone(), t);
                            }
                        }
                    }
                    self.infer_closure_return_type(final_expr, &extended)
                } else {
                    self.context.i64_type().into()
                }
            }
            ExprKind::If {
                then_block,
                else_branch,
                ..
            } => {
                if let Some(else_expr) = else_branch {
                    self.infer_closure_return_type(else_expr, param_types)
                } else if let Some(final_expr) = &then_block.final_expr {
                    self.infer_closure_return_type(final_expr, param_types)
                } else {
                    self.context.i64_type().into()
                }
            }
            // B-2026-07-13-20 sibling: a `match` tail (`|d| match d { … => f"x", …
            // }`) had NO arm here, so it fell to the `i64` default while the body
            // returned the (heap) arm value → LLVM verifier "return type does not
            // match operand type of return inst". All arms have the same type by
            // typecheck, so infer from the first arm's body (a block-body arm
            // recurses through the Block arm above).
            ExprKind::Match { arms, .. } => arms
                .first()
                .map(|a| self.infer_closure_return_type(&a.body, param_types))
                .unwrap_or_else(|| self.context.i64_type().into()),
            ExprKind::Tuple(elems) => {
                let field_types: Vec<BasicTypeEnum<'ctx>> = elems
                    .iter()
                    .map(|e| self.infer_closure_return_type(e, param_types))
                    .collect();
                self.context.struct_type(&field_types, false).into()
            }
            // Calls: look up in module or use i64 fallback.
            ExprKind::Call { callee, args } => {
                if let ExprKind::Identifier(fname) = &callee.kind {
                    // B-2026-07-13-20 sibling: the bare `Option`/`Result`
                    // constructors (`Some(x)` / `Ok(x)` / `Err(x)`) evaluate to
                    // the wrapper enum's type-erased layout, not the payload type
                    // and not the `i64` fallback — a closure tail `|n| if n>0 {
                    // Some(f"..") } else { None }` otherwise declared `-> i64`
                    // against a 4-word Option body (verifier failure).
                    match fname.as_str() {
                        "Some" | "None" => {
                            if let Some(l) = self.type_decls.enum_layouts.get("Option") {
                                return l.llvm_type.into();
                            }
                        }
                        "Ok" | "Err" => {
                            if let Some(l) = self.type_decls.enum_layouts.get("Result") {
                                return l.llvm_type.into();
                            }
                        }
                        _ => {}
                    }
                    if let Some(f) = self.module.get_function(fname) {
                        return f
                            .get_type()
                            .get_return_type()
                            .unwrap_or_else(|| self.context.i64_type().into());
                    }
                    // B-2026-07-15-8: a call to a captured CLOSURE variable
                    // (`|s| wrap(wrap(s))` where `wrap` is another closure) —
                    // `wrap` is not a module fn, so the check above misses and
                    // the body's real return type (`wrap`'s return, here a
                    // `String`) fell to the i64 default. The enclosing closure
                    // fn was then declared `-> i64` while its body returned the
                    // `{ptr,i64,i64}` String → LLVM verifier "return type does
                    // not match operand type of return inst". `closure_fn_types`
                    // still holds the outer entries at inference time (the
                    // body-scope `take` happens later in `compile_closure`), so
                    // the callee's env-first `FunctionType` is visible here; its
                    // real return type is the closure's declared result. `None`
                    // return (a unit closure) keeps the i64 placeholder.
                    if let Some(ft) = self.closure_state.closure_fn_types.get(fname) {
                        if let Some(ret) = ft.get_return_type() {
                            return ret;
                        }
                    }
                }
                // Lowered operator dispatch: `<Primitive>.<op>(args)` —
                // the lowering pass produces these from BinOp/UnaryOp.
                if let ExprKind::Path { segments, .. } = &callee.kind {
                    if segments.len() == 2 {
                        let target = segments[0].as_str();
                        let method = segments[1].as_str();
                        // Collection / String constructor (`Vec.new()`,
                        // `String.from(x)`, `Map.new()`, …) evaluates to the
                        // container's own heap type — a `{ptr,len,cap}` aggregate
                        // for Vec/String, an opaque handle `ptr` for Map/Set — NOT
                        // the i64 fallback below. B-2026-07-13-20: a closure whose
                        // BLOCK-body tail resolves to `let v = Vec.new(); …; v`
                        // (no type annotation) inferred `i64` here, so the closure
                        // fn was declared `-> i64` while its body returned the Vec
                        // aggregate → LLVM verifier "return type does not match
                        // operand type of return inst". A single-EXPRESSION body
                        // (`|| make()`) already resolved via the module-fn return
                        // type; only the block-body-tail-constructor path missed.
                        if matches!(method, "new" | "with_capacity" | "from" | "from_vec") {
                            match target {
                                "Vec" | "VecDeque" | "String" | "CString" => {
                                    return self.vec_struct_type().into();
                                }
                                "Map" | "HashMap" | "Set" | "HashSet" | "SortedMap"
                                | "SortedSet" => {
                                    return self.context.ptr_type(AddressSpace::default()).into();
                                }
                                _ => {}
                            }
                        }
                        // Enum-variant construction: `Enum.Variant(args)`
                        // (Result.Ok/Err, Option.Some, user enums) returns
                        // the enum's type-erased LLVM layout, NOT the
                        // payload type. Guard on the variant actually
                        // existing (`tags`) so a static method like
                        // `Enum.from(..)` doesn't get mis-typed. Without
                        // this the closure fn's declared return type
                        // collapses to the i64 fallback below while the
                        // compiled body produces the full enum struct —
                        // LLVM verifier: "return type does not match
                        // operand type of return inst".
                        if let Some(layout) = self.type_decls.enum_layouts.get(target) {
                            if layout.tags.contains_key(method) {
                                return BasicTypeEnum::StructType(layout.llvm_type);
                            }
                        }
                        // Eq/Ord methods return bool regardless of operand type.
                        if matches!(method, "eq" | "ne" | "lt" | "le" | "gt" | "ge") {
                            return self.context.bool_type().into();
                        }
                        // Arithmetic, bitwise, shifts, not — return Self.
                        let is_self_returning = matches!(
                            method,
                            "add"
                                | "sub"
                                | "mul"
                                | "div"
                                | "rem"
                                | "neg"
                                | "bitand"
                                | "bitor"
                                | "bitxor"
                                | "shl"
                                | "shr"
                                | "not"
                        );
                        if is_self_returning {
                            return match target {
                                "f32" => self.context.f32_type().into(),
                                "f64" => self.context.f64_type().into(),
                                "bool" => self.context.bool_type().into(),
                                _ => {
                                    // Fall back to inferring from operand if available.
                                    if let Some(arg) = args.first() {
                                        return self
                                            .infer_closure_return_type(&arg.value, param_types);
                                    }
                                    self.context.i64_type().into()
                                }
                            };
                        }
                    }
                }
                self.context.i64_type().into()
            }
            // Bare enum-variant path expression: a unit variant used as a
            // value, e.g. `Option.None` / `Result`-less user unit variants
            // (no call args). Same type-erased-layout rule as the
            // `Enum.Variant(args)` call form above.
            ExprKind::Path { segments, .. } if segments.len() == 2 => {
                if let Some(layout) = self.type_decls.enum_layouts.get(segments[0].as_str()) {
                    if layout.tags.contains_key(segments[1].as_str()) {
                        return BasicTypeEnum::StructType(layout.llvm_type);
                    }
                }
                self.context.i64_type().into()
            }
            // A closure body that is ITSELF a closure (currying:
            // `|n| |x| x + n`) evaluates to a closure fat pointer
            // `{ fn_ptr, env_ptr }`, not the `i64` default. Without this the
            // outer closure fn is declared `-> i64` while its body returns the
            // fat-pointer struct → LLVM verifier "return type does not match
            // operand type of return inst" (B-2026-07-12-12). The escaping
            // inner env is heap-allocated per outer call (see the tail-heap-
            // closure marking in `compile_closure`) so distinct instances
            // (`make(5)` / `make(10)`) don't alias one reused stack env.
            ExprKind::Closure { .. } => self.closure_value_type().into(),
            // A struct-literal body (`|| Point { x, y }`) evaluates to the
            // struct's LLVM aggregate, NOT the `i64` default — without this the
            // closure fn is declared `-> i64` while its body returns the struct
            // (LLVM verifier "return type does not match operand type of return
            // inst"), which blocked an aggregate `OnceLock.get_or_init(|| S {..})`
            // (B-2026-07-12-2). Prefer the concrete mono instantiation (a
            // generic `Wrap { .. }` literal) recorded by the lowering pass, then
            // the base `struct_types` shape, then the enum-layout for a struct-
            // VARIANT literal; fall back to `i64` only when nothing resolves.
            ExprKind::StructLiteral { path, .. } => self
                .struct_inst_mono_type_for_expr(expr)
                .map(Into::into)
                .or_else(|| {
                    path.last().and_then(|n| {
                        self.type_decls
                            .struct_types
                            .get(n.as_str())
                            .map(|st| (*st).into())
                            .or_else(|| {
                                self.type_decls
                                    .enum_layouts
                                    .get(n.as_str())
                                    .map(|l| l.llvm_type.into())
                            })
                    })
                })
                .unwrap_or_else(|| self.context.i64_type().into()),
            _ => self.context.i64_type().into(),
        }
    }

    // ── Disjoint-capture slice 4 helpers ───────────────────────────

    /// Build a per-path env layout for the closure at `closure_span`.
    /// Returns `None` when the ownership pass did not supply path-mode
    /// data for this closure (caller falls back to per-name layout).
    /// Roots that aren't safe for path-precise stitching (RC-fallback
    /// promoted, `ref`-param-shaped, or any projection step the resolver
    /// can't walk through `struct_field_names`) are collapsed to a single
    /// whole-root slot for that root — other roots in the same layout
    /// still get path-precise slots.
    fn build_capture_path_layout(
        &self,
        closure_span: &Span,
        free_vars: &[String],
    ) -> Option<CapturePathLayout<'ctx>> {
        let key = SpanKey::from_span(closure_span);
        let path_modes = self.closure_state.closure_capture_paths.get(&key)?;

        // Group paths by root, preserving the slice-2 list order so
        // multiple paths under the same root keep deterministic ordering.
        let mut roots_in_order: Vec<String> = Vec::new();
        let mut by_root: HashMap<String, Vec<&CapturePath>> = HashMap::new();
        for (path, _mode) in path_modes {
            if !self.variables.contains_key(path.root.as_str()) {
                // Path references a binding the codegen scope doesn't
                // know about (e.g. captured by a nested closure but
                // shadowed before reaching this point) — skip; the
                // legacy per-name walker mirrors the same filter.
                continue;
            }
            if !by_root.contains_key(&path.root) {
                roots_in_order.push(path.root.clone());
            }
            by_root.entry(path.root.clone()).or_default().push(path);
        }
        // The slice-2 path set is keyed off the closure's free-variable
        // scan, which records roots even when the body only reaches them
        // through stopping constructs. Cross-check with `free_vars` so
        // any root the per-name walker found but slice 2 missed (and
        // vice-versa) doesn't silently drop from the env — fall back to
        // per-name layout if the two sets disagree.
        let path_root_set: HashSet<&String> = by_root.keys().collect();
        let free_var_set: HashSet<&String> = free_vars.iter().collect();
        if path_root_set != free_var_set {
            return None;
        }

        let mut slot_tys: Vec<BasicTypeEnum<'ctx>> = Vec::new();
        let mut slot_sources: Vec<(String, Vec<u32>)> = Vec::new();
        let mut root_plans: Vec<(String, RootUnpackPlan<'ctx>)> = Vec::new();

        for root in roots_in_order {
            let slot = *self.variables.get(root.as_str())?;
            let type_name = self.var_types.var_type_names.get(root.as_str()).cloned();
            let paths = by_root.get(&root).unwrap();

            // Conservative force-whole-root triggers: RC-fallback root
            // (slot.ty is `ptr`, body field-access goes through the
            // heap-deref path), ref-param root (alloca holds a pointer,
            // not a struct value), or any path under this root has a
            // projection chain that can't be resolved through
            // `struct_field_names`.
            let force_whole_root = self.is_rc_fallback_binding(&root)
                || self.borrow_vars.ref_params.contains_key(root.as_str())
                || paths.iter().any(|p| {
                    !p.projection.is_empty()
                        && self
                            .resolve_gep_chain(slot.ty, type_name.as_deref(), &p.projection)
                            .is_none()
                });

            let any_whole = paths.iter().any(|p| p.projection.is_empty());

            if force_whole_root || any_whole {
                // One whole-root slot for this root. Drop sub-paths —
                // the body walks the whole root and field reads work
                // through normal compile_field_access dispatch.
                let slot_idx = slot_tys.len();
                slot_tys.push(slot.ty);
                slot_sources.push((root.clone(), Vec::new()));
                root_plans.push((
                    root.clone(),
                    RootUnpackPlan {
                        root_ty: slot.ty,
                        type_name,
                        whole_root_slot: Some(slot_idx),
                        sub_slots: Vec::new(),
                    },
                ));
            } else {
                // Per-path: one slot per non-empty projection. The slice-2
                // set guarantees every path here has non-empty projection
                // (`any_whole` is false in this branch).
                let mut sub_slots: Vec<(usize, Vec<u32>, BasicTypeEnum<'ctx>)> = Vec::new();
                for p in paths {
                    let gep_chain = self
                        .resolve_gep_chain(slot.ty, type_name.as_deref(), &p.projection)
                        .unwrap();
                    let leaf_ty = self.leaf_type_for_chain(slot.ty, &gep_chain);
                    let slot_idx = slot_tys.len();
                    slot_tys.push(leaf_ty);
                    slot_sources.push((root.clone(), gep_chain.clone()));
                    sub_slots.push((slot_idx, gep_chain, leaf_ty));
                }
                root_plans.push((
                    root.clone(),
                    RootUnpackPlan {
                        root_ty: slot.ty,
                        type_name,
                        whole_root_slot: None,
                        sub_slots,
                    },
                ));
            }
        }

        Some(CapturePathLayout {
            slot_tys,
            slot_sources,
            root_plans,
        })
    }

    /// Walk a projection chain (root-to-leaf field names, possibly mixed
    /// with numeric tuple indices) into a sequence of LLVM struct GEP
    /// indices. Returns `None` if any step can't be resolved — the
    /// caller treats that root as a whole-root capture. `type_name` is
    /// the source-level type of the root, looked up in
    /// `struct_field_names` to translate field-name → index.
    fn resolve_gep_chain(
        &self,
        root_ty: BasicTypeEnum<'ctx>,
        type_name: Option<&str>,
        projection: &[String],
    ) -> Option<Vec<u32>> {
        let mut current_ty = root_ty;
        let mut current_type_name: Option<String> = type_name.map(|s| s.to_string());
        let mut chain: Vec<u32> = Vec::with_capacity(projection.len());
        for step in projection {
            let struct_ty = match current_ty {
                BasicTypeEnum::StructType(st) => st,
                _ => return None,
            };
            // Try struct-field-name → index lookup first.
            let idx = if let Some(name) = current_type_name.as_deref() {
                if let Some(names) = self.type_decls.struct_field_names.get(name) {
                    names.iter().position(|f| f == step).map(|p| p as u32)
                } else {
                    None
                }
            } else {
                None
            };
            // Fall back to numeric tuple-index parse.
            let idx = idx.or_else(|| step.parse::<u32>().ok())?;
            // Advance the LLVM and source type-name pointers.
            current_ty = struct_ty.get_field_type_at_index(idx)?;
            current_type_name = current_type_name
                .as_deref()
                .and_then(|name| self.type_decls.struct_field_type_names.get(name))
                .and_then(|tys| tys.get(idx as usize).cloned())
                .flatten();
            chain.push(idx);
        }
        Some(chain)
    }

    /// Resolve the LLVM type at the end of a GEP chain rooted at
    /// `root_ty`. Used by both the capture-site loader (to type the load
    /// from the source root) and the unpack-site stitcher (to type the
    /// store into the stitched root).
    fn leaf_type_for_chain(
        &self,
        root_ty: BasicTypeEnum<'ctx>,
        chain: &[u32],
    ) -> BasicTypeEnum<'ctx> {
        let mut current = root_ty;
        for &idx in chain {
            if let BasicTypeEnum::StructType(st) = current {
                current = st.get_field_type_at_index(idx).unwrap();
            } else {
                // Builder guarantees the chain is resolvable; this branch
                // is only reached if a non-struct sneaks in, which would
                // be a bug — return the i64 fallback rather than panic.
                return self.context.i64_type().into();
            }
        }
        current
    }

    /// GEP into a struct alloca via a chain of field indices. Used by
    /// both the capture site (to read a leaf from the outer-scope root)
    /// and the unpack site (to write a leaf into the stitched-back
    /// root). The chain is rooted at field index 0 conceptually — every
    /// `struct_gep` step walks down one level from the current pointer.
    fn gep_root_chain(
        &self,
        root_ty: BasicTypeEnum<'ctx>,
        root_ptr: inkwell::values::PointerValue<'ctx>,
        chain: &[u32],
    ) -> inkwell::values::PointerValue<'ctx> {
        let mut current_ptr = root_ptr;
        let mut current_ty = root_ty;
        for (i, &idx) in chain.iter().enumerate() {
            let struct_ty = match current_ty {
                BasicTypeEnum::StructType(st) => st,
                _ => return current_ptr,
            };
            current_ptr = self
                .builder
                .build_struct_gep(struct_ty, current_ptr, idx, &format!("cap.gep.{}", i))
                .unwrap();
            current_ty = struct_ty.get_field_type_at_index(idx).unwrap();
        }
        current_ptr
    }

    /// Collect the names of variables captured by a closure (free variables from outer scope).
    ///
    /// A variable is captured if:
    /// 1. It is referenced in `body`.
    /// 2. It is NOT one of the closure's own parameters.
    /// 3. It is NOT defined by a `let` inside the closure body.
    /// 4. It IS present in the current outer scope (`self.variables`).
    pub(super) fn collect_closure_free_vars(
        &self,
        params: &[ClosureParam],
        body: &Expr,
    ) -> Vec<String> {
        let param_names: HashSet<String> = params
            .iter()
            .flat_map(|p| p.pattern.binding_names())
            .collect();

        let mut refs = HashSet::new();
        let mut inner_defs = HashSet::new();
        self.refs_in_expr(body, &mut refs, &mut inner_defs);

        let mut free: Vec<String> = refs
            .into_iter()
            .filter(|n| !param_names.contains(n) && !inner_defs.contains(n))
            .filter(|n| self.variables.contains_key(n.as_str()))
            .collect();
        free.sort(); // deterministic order
        free
    }

    /// Walk `expr` and collect all identifier references into `refs`, and all
    /// names bound by `let` statements into `defs`. Delegates to the shared
    /// plain-AST walker in `crate::closure_escape` (also used by the escape
    /// analysis — one walk, no drift).
    pub(super) fn refs_in_expr(
        &self,
        expr: &Expr,
        refs: &mut HashSet<String>,
        defs: &mut HashSet<String>,
    ) {
        crate::closure_escape::refs_in_expr(expr, refs, defs)
    }

    /// Block sibling of [`Self::refs_in_expr`].
    pub(super) fn refs_in_block(
        &self,
        block: &Block,
        refs: &mut HashSet<String>,
        defs: &mut HashSet<String>,
    ) {
        crate::closure_escape::refs_in_block(block, refs, defs)
    }
}
