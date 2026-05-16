//! Control-flow codegen: for, while, loop, if, if-let, match, labeled
//! blocks, break, continue, plus the bounds-check elision plumbing.
//!
//! Houses every per-source-construct compiler that establishes basic
//! blocks for control transfer — the `compile_for_*` family,
//! `compile_if` / `compile_if_let`, `compile_while`, `compile_loop`,
//! `compile_labeled_block`, `compile_break` / `compile_continue`,
//! plus `compile_match` and its supporting machinery
//! (`scrutinee_is_borrow_call`, `compile_pattern_condition`,
//! `extract_enum_tag`, `enum_tag_for_variant`, `enum_type_for_variant`,
//! `pattern_payload_word_count`, `pattern_payload_llvm_type`,
//! `reconstruct_payload_value`). Also houses the BCE-related
//! `collect_asserted_bounds_*` / `walk_guard_conjuncts` /
//! `extract_index_bound_from_binop` / `resolve_len_origin`,
//! `resolve_slice_source` / `load_slice_pattern_element` /
//! `compile_slice_pattern_condition` / `bind_slice_pattern`, and
//! `compile_print`.

use crate::ast::*;

use inkwell::types::BasicTypeEnum;
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, IntValue};
use inkwell::AddressSpace;
use inkwell::IntPredicate;

use super::state::{AssertedIndexBound, LoopFrame, SliceSource, VarSlot};

impl<'ctx> super::Codegen<'ctx> {
    // ── IfLet ────────────────────────────────────────────────────

    pub(super) fn compile_if_let(
        &mut self,
        pattern: &Pattern,
        value: &Expr,
        then_block: &Block,
        else_branch: Option<&Expr>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let val = self.compile_expr(value)?;
        let cond = self.compile_pattern_condition(pattern, val)?;
        // Reuse if-else codegen
        let fn_val = self.current_fn.unwrap();
        let then_bb = self.context.append_basic_block(fn_val, "iflet.then");
        let else_bb = self.context.append_basic_block(fn_val, "iflet.else");
        let merge_bb = self.context.append_basic_block(fn_val, "iflet.merge");

        self.builder
            .build_conditional_branch(cond.into_int_value(), then_bb, else_bb)
            .unwrap();

        self.builder.position_at_end(then_bb);
        self.bind_pattern_values(pattern, val)?;
        let then_val = self.compile_block(then_block)?;
        let then_terminated = self
            .builder
            .get_insert_block()
            .unwrap()
            .get_terminator()
            .is_some();
        let then_end = self.builder.get_insert_block().unwrap();
        if !then_terminated {
            self.builder.build_unconditional_branch(merge_bb).unwrap();
        }

        self.builder.position_at_end(else_bb);
        let else_val = if let Some(eb) = else_branch {
            match &eb.kind {
                ExprKind::Block(blk) => self.compile_block(blk)?,
                _ => Some(self.compile_expr(eb)?),
            }
        } else {
            None
        };
        let else_terminated = self
            .builder
            .get_insert_block()
            .unwrap()
            .get_terminator()
            .is_some();
        let else_end = self.builder.get_insert_block().unwrap();
        if !else_terminated {
            self.builder.build_unconditional_branch(merge_bb).unwrap();
        }

        self.builder.position_at_end(merge_bb);
        if let (Some(tv), Some(ev)) = (&then_val, &else_val) {
            if !then_terminated && !else_terminated && tv.get_type() == ev.get_type() {
                let phi = self.builder.build_phi(tv.get_type(), "ifletval").unwrap();
                phi.add_incoming(&[(tv, then_end), (ev, else_end)]);
                return Ok(phi.as_basic_value());
            }
        }
        Ok(self.context.i64_type().const_int(0, false).into())
    }

    pub(super) fn compile_print(
        &mut self,
        name: &str,
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let zero = self.context.i64_type().const_int(0, false);
        if args.is_empty() {
            let fmt = self.builder.build_global_string_ptr("\n", "nl").unwrap();
            self.builder
                .build_call(
                    self.printf_fn,
                    &[BasicMetadataValueEnum::from(fmt.as_pointer_value())],
                    "printf",
                )
                .unwrap();
            return Ok(zero.into());
        }

        let nl = if name == "println" { "\n" } else { "" };

        // Collection dispatch: when the print arg is a bare identifier that
        // we've registered as a Vec or Map variable, emit a call to the
        // per-type Display fn against the variable's alloca. This is the
        // primary path for `println(v)` on collections; it produces the same
        // formatted output the interpreter prints. Bare Vec/Map values appear
        // as struct/pointer values in the legacy `is_struct_value` /
        // `is_pointer_value` arms below — that path is wrong for collections
        // (Vec gets treated as String; Map gets printed as a raw address) —
        // but those arms are still reachable for non-identifier expressions
        // (function returns, fresh literals) where the source-level type is
        // not in the side-tables, so we leave them in place as fallbacks.
        if let ExprKind::Identifier(var_name) = &args[0].value.kind {
            // Vec[T]: side-table both `vec_elem_types` and `var_elem_type_exprs`
            // are set (the latter is what distinguishes a Vec variable from a
            // String variable, which only sets `vec_elem_types`).
            if self.vec_elem_types.contains_key(var_name)
                && self.var_elem_type_exprs.contains_key(var_name)
            {
                let elem_te = self.var_elem_type_exprs[var_name].clone();
                let slot = self
                    .variables
                    .get(var_name)
                    .copied()
                    .ok_or_else(|| format!("compile_print: '{var_name}' not bound"))?;
                let display_fn = self.emit_vec_display_fn_te(&elem_te);
                self.builder
                    .build_call(display_fn, &[slot.ptr.into()], "vd")
                    .unwrap();
                if !nl.is_empty() {
                    let nl_str = self.builder.build_global_string_ptr("\n", "vd.nl").unwrap();
                    self.builder
                        .build_call(self.printf_fn, &[nl_str.as_pointer_value().into()], "p")
                        .unwrap();
                }
                return Ok(zero.into());
            }
            // Map[K, V]: side-tables hold both K and V `TypeExpr`s.
            if self.map_key_type_exprs.contains_key(var_name)
                && self.var_elem_type_exprs.contains_key(var_name)
            {
                let k_te = self.map_key_type_exprs[var_name].clone();
                let v_te = self.var_elem_type_exprs[var_name].clone();
                let slot = self
                    .variables
                    .get(var_name)
                    .copied()
                    .ok_or_else(|| format!("compile_print: '{var_name}' not bound"))?;
                let display_fn = self.emit_map_display_fn(&k_te, &v_te);
                self.builder
                    .build_call(display_fn, &[slot.ptr.into()], "md")
                    .unwrap();
                if !nl.is_empty() {
                    let nl_str = self.builder.build_global_string_ptr("\n", "md.nl").unwrap();
                    self.builder
                        .build_call(self.printf_fn, &[nl_str.as_pointer_value().into()], "p")
                        .unwrap();
                }
                return Ok(zero.into());
            }
            // Set[T]: side-table holds the element `TypeExpr`.
            if self.set_elem_type_exprs.contains_key(var_name) {
                let elem_te = self.set_elem_type_exprs[var_name].clone();
                let slot = self
                    .variables
                    .get(var_name)
                    .copied()
                    .ok_or_else(|| format!("compile_print: '{var_name}' not bound"))?;
                let display_fn = self.emit_set_display_fn(&elem_te);
                self.builder
                    .build_call(display_fn, &[slot.ptr.into()], "sd")
                    .unwrap();
                if !nl.is_empty() {
                    let nl_str = self.builder.build_global_string_ptr("\n", "sd.nl").unwrap();
                    self.builder
                        .build_call(self.printf_fn, &[nl_str.as_pointer_value().into()], "p")
                        .unwrap();
                }
                return Ok(zero.into());
            }
        }

        let val = self.compile_expr(&args[0].value)?;

        if val.is_int_value() {
            let bits = val.into_int_value().get_type().get_bit_width();
            if bits == 1 {
                let true_s = self
                    .builder
                    .build_global_string_ptr(&format!("true{nl}"), "ts")
                    .unwrap();
                let false_s = self
                    .builder
                    .build_global_string_ptr(&format!("false{nl}"), "fs")
                    .unwrap();
                let sel = self
                    .builder
                    .build_select(
                        val.into_int_value(),
                        true_s.as_pointer_value(),
                        false_s.as_pointer_value(),
                        "bstr",
                    )
                    .unwrap();
                self.builder
                    .build_call(
                        self.printf_fn,
                        &[BasicMetadataValueEnum::from(sel.into_pointer_value())],
                        "printf",
                    )
                    .unwrap();
            } else {
                let fmt = self
                    .builder
                    .build_global_string_ptr(&format!("%lld{nl}"), "fi")
                    .unwrap();
                self.builder
                    .build_call(
                        self.printf_fn,
                        &[
                            BasicMetadataValueEnum::from(fmt.as_pointer_value()),
                            BasicMetadataValueEnum::from(val.into_int_value()),
                        ],
                        "printf",
                    )
                    .unwrap();
            }
        } else if val.is_struct_value() {
            // String struct { ptr, i64, i64 } — extract the data pointer for printf %s.
            let str_ptr = self
                .builder
                .build_extract_value(val.into_struct_value(), 0, "str.ptr")
                .unwrap()
                .into_pointer_value();
            let fmt = self
                .builder
                .build_global_string_ptr(&format!("%s{nl}"), "fsp")
                .unwrap();
            self.builder
                .build_call(
                    self.printf_fn,
                    &[
                        BasicMetadataValueEnum::from(fmt.as_pointer_value()),
                        BasicMetadataValueEnum::from(str_ptr),
                    ],
                    "printf",
                )
                .unwrap();
        } else if val.is_pointer_value() {
            // Raw pointer (shared types, etc.) — pass directly to %s.
            let fmt = self
                .builder
                .build_global_string_ptr(&format!("%s{nl}"), "fsp")
                .unwrap();
            self.builder
                .build_call(
                    self.printf_fn,
                    &[
                        BasicMetadataValueEnum::from(fmt.as_pointer_value()),
                        BasicMetadataValueEnum::from(val.into_pointer_value()),
                    ],
                    "printf",
                )
                .unwrap();
        } else if val.is_float_value() {
            let fmt = self
                .builder
                .build_global_string_ptr(&format!("%g{nl}"), "ff")
                .unwrap();
            self.builder
                .build_call(
                    self.printf_fn,
                    &[
                        BasicMetadataValueEnum::from(fmt.as_pointer_value()),
                        BasicMetadataValueEnum::from(val.into_float_value()),
                    ],
                    "printf",
                )
                .unwrap();
        }
        Ok(zero.into())
    }

    // ── Control flow ──────────────────────────────────────────────

    pub(super) fn compile_if(
        &mut self,
        condition: &Expr,
        then_block: &Block,
        else_branch: Option<&Expr>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let cond_val = self.compile_expr(condition)?.into_int_value();
        let fn_val = self.current_fn.unwrap();
        let then_bb = self.context.append_basic_block(fn_val, "then");
        let else_bb = self.context.append_basic_block(fn_val, "else");
        let merge_bb = self.context.append_basic_block(fn_val, "ifmerge");

        self.builder
            .build_conditional_branch(cond_val, then_bb, else_bb)
            .unwrap();

        self.builder.position_at_end(then_bb);
        let then_val = self.compile_block(then_block)?;
        let then_terminated = self
            .builder
            .get_insert_block()
            .unwrap()
            .get_terminator()
            .is_some();
        let then_end_bb = self.builder.get_insert_block().unwrap();
        if !then_terminated {
            self.builder.build_unconditional_branch(merge_bb).unwrap();
        }

        self.builder.position_at_end(else_bb);
        let else_val = if let Some(else_expr) = else_branch {
            match &else_expr.kind {
                ExprKind::Block(blk) => self.compile_block(blk)?,
                ExprKind::If {
                    condition: c,
                    then_block: tb,
                    else_branch: eb,
                } => {
                    let v = self.compile_if(c, tb, eb.as_deref())?;
                    Some(v)
                }
                _ => {
                    let v = self.compile_expr(else_expr)?;
                    Some(v)
                }
            }
        } else {
            None
        };
        let else_terminated = self
            .builder
            .get_insert_block()
            .unwrap()
            .get_terminator()
            .is_some();
        let else_end_bb = self.builder.get_insert_block().unwrap();
        if !else_terminated {
            self.builder.build_unconditional_branch(merge_bb).unwrap();
        }

        self.builder.position_at_end(merge_bb);

        if let (Some(tv), Some(ev)) = (&then_val, &else_val) {
            if !then_terminated && !else_terminated && tv.get_type() == ev.get_type() {
                let phi = self.builder.build_phi(tv.get_type(), "ifval").unwrap();
                phi.add_incoming(&[(tv, then_end_bb), (ev, else_end_bb)]);
                return Ok(phi.as_basic_value());
            }
        }

        Ok(self.context.i64_type().const_int(0, false).into())
    }

    pub(super) fn compile_while(
        &mut self,
        label: Option<&str>,
        condition: &Expr,
        body: &Block,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let fn_val = self.current_fn.unwrap();
        let cond_bb = self.context.append_basic_block(fn_val, "while.cond");
        let body_bb = self.context.append_basic_block(fn_val, "while.body");
        let exit_bb = self.context.append_basic_block(fn_val, "while.exit");

        self.builder.build_unconditional_branch(cond_bb).unwrap();

        self.loop_stack.push(LoopFrame {
            label: label.map(str::to_string),
            continue_bb: cond_bb,
            break_bb: exit_bb,
            result_slot: None,
        });

        self.builder.position_at_end(cond_bb);
        let cond_val = self.compile_expr(condition)?.into_int_value();
        self.builder
            .build_conditional_branch(cond_val, body_bb, exit_bb)
            .unwrap();

        self.builder.position_at_end(body_bb);
        // Bounds-check-elision: the guard is true at body entry, so every
        // signed comparison conjunct that establishes an index bound can be
        // pushed as an asserted fact. `compile_vec_index` consults the stack
        // and drops the matching half of its runtime bounds check.
        let pushed_bounds = self.collect_asserted_bounds_from_guard(condition);
        let pushed_count = pushed_bounds.len();
        self.asserted_index_bounds.extend(pushed_bounds);
        self.compile_block(body)?;
        // Pop the bounds we pushed for this loop; restore the surrounding
        // scope's stack untouched. Nested loops therefore see only their
        // own and outer-loop bounds, never inner-loop leftovers.
        for _ in 0..pushed_count {
            self.asserted_index_bounds.pop();
        }
        if self
            .builder
            .get_insert_block()
            .unwrap()
            .get_terminator()
            .is_none()
        {
            self.builder.build_unconditional_branch(cond_bb).unwrap();
        }

        self.loop_stack.pop();
        self.builder.position_at_end(exit_bb);
        Ok(self.context.i64_type().const_int(0, false).into())
    }

    /// Walk a boolean expression that holds true at the entry to a body
    /// block (e.g. a `while` guard or an `if` cond) and return the
    /// index-safety facts it asserts. Only handles `and`-chained signed
    /// comparisons against identifiers or zero — the conservative subset
    /// that the kata-5 elision pass needs. Unrecognized shapes are silently
    /// ignored (the bounds check stays as-is for the corresponding index).
    pub(super) fn collect_asserted_bounds_from_guard(
        &self,
        cond: &Expr,
    ) -> Vec<AssertedIndexBound> {
        let mut out = Vec::new();
        self.walk_guard_conjuncts(cond, &mut out);
        out
    }

    /// Asserted-bounds facts for the body of `for i in start..end`. The
    /// for-range loop natively establishes `start <= i < end` (or `<= end`
    /// for inclusive), so we can short-cut the guard-parsing surface for
    /// the common `for i in 0..v.len()` and `for i in 1..n` shapes.
    ///
    /// Lower bound: pushed when `start` is None (defaults to 0) or a
    /// non-negative integer literal. Anything else (a variable, an
    /// arithmetic expression) is conservative — we don't know its sign
    /// without range analysis, so no LowerBound fact.
    ///
    /// Upper bound: pushed only for exclusive ranges (`0..end`, not
    /// `0..=end`) when `end` resolves to a Vec or Slice's `.len()` via
    /// `resolve_len_origin`. Inclusive ranges include the end value
    /// itself, which would be one past the last valid index — proving
    /// `i < v.len()` inside the body would require knowing `end <
    /// v.len()`, which the source rarely makes explicit.
    pub(super) fn collect_asserted_bounds_from_for_range(
        &self,
        pattern: &Pattern,
        start: &Option<Box<Expr>>,
        end: &Option<Box<Expr>>,
        inclusive: bool,
    ) -> Vec<AssertedIndexBound> {
        let idx_var = match &pattern.kind {
            PatternKind::Binding(name) => name.clone(),
            _ => return Vec::new(),
        };
        let mut out = Vec::new();
        let lower_proven = match start.as_deref().map(|e| &e.kind) {
            None => true,
            Some(ExprKind::Integer(n, _)) if *n >= 0 => true,
            _ => false,
        };
        if lower_proven {
            out.push(AssertedIndexBound::LowerBound {
                idx_var: idx_var.clone(),
            });
        }
        if !inclusive {
            if let Some(e) = end.as_deref() {
                if let Some(vec_var) = self.resolve_len_origin(e) {
                    out.push(AssertedIndexBound::UpperBound { idx_var, vec_var });
                }
            }
        }
        out
    }

    pub(super) fn walk_guard_conjuncts(&self, cond: &Expr, out: &mut Vec<AssertedIndexBound>) {
        if let ExprKind::Binary { op, left, right } = &cond.kind {
            // Recurse through `and`-chained conjuncts so multi-clause
            // guards like `lo >= 0 and hi < n and chars[lo] == chars[hi]`
            // contribute each conjunct's fact independently.
            if matches!(op, BinOp::And) {
                self.walk_guard_conjuncts(left, out);
                self.walk_guard_conjuncts(right, out);
                return;
            }
            if let Some(fact) = self.extract_index_bound_from_binop(op, left, right) {
                out.push(fact);
            }
        }
        // The typechecker rewrites integer comparisons through trait-method
        // dispatch (e.g. `lo >= 0` → `i64::ge(lo, 0)`), so the post-lowering
        // AST carries `>=` / `<=` / sometimes `<` / `>` as `Call` nodes whose
        // callee is a `Path { segments: ["<int>", "ge"|"le"|"lt"|"gt"], .. }`.
        // The Binary form above still handles the cases the lowering leaves
        // alone (which empirically includes `<` between two same-typed i64s);
        // this Call arm catches the rest.
        if let ExprKind::Call { callee, args } = &cond.kind {
            if let ExprKind::Path { segments, .. } = &callee.kind {
                if segments.len() == 2 && args.len() == 2 {
                    let op = match segments[1].as_str() {
                        "ge" => Some(BinOp::GtEq),
                        "le" => Some(BinOp::LtEq),
                        "lt" => Some(BinOp::Lt),
                        "gt" => Some(BinOp::Gt),
                        _ => None,
                    };
                    if let Some(op) = op {
                        if let Some(fact) =
                            self.extract_index_bound_from_binop(&op, &args[0].value, &args[1].value)
                        {
                            out.push(fact);
                        }
                    }
                }
            }
        }
    }

    /// Match a single binary comparison and decode whichever index-safety
    /// fact (if any) it establishes. Recognizes the four normal forms
    /// the kata's `while`-guard surface produces:
    ///   - `idx >= 0`  /  `0 <= idx`           → LowerBound { idx }
    ///   - `idx < vec.len()`                    → UpperBound { idx, vec }
    ///   - `idx < n` where n aliases vec.len()  → UpperBound { idx, vec }
    ///
    /// Strict-less only — `idx <= n-1` would be sound but isn't a shape
    /// the kata surface produces, and conservatively skipping it now keeps
    /// the elision predicate small.
    pub(super) fn extract_index_bound_from_binop(
        &self,
        op: &BinOp,
        left: &Expr,
        right: &Expr,
    ) -> Option<AssertedIndexBound> {
        match op {
            // `idx >= 0`
            BinOp::GtEq => {
                if let (ExprKind::Identifier(idx), ExprKind::Integer(0, _)) =
                    (&left.kind, &right.kind)
                {
                    return Some(AssertedIndexBound::LowerBound {
                        idx_var: idx.clone(),
                    });
                }
                None
            }
            // `0 <= idx`
            BinOp::LtEq => {
                if let (ExprKind::Integer(0, _), ExprKind::Identifier(idx)) =
                    (&left.kind, &right.kind)
                {
                    return Some(AssertedIndexBound::LowerBound {
                        idx_var: idx.clone(),
                    });
                }
                None
            }
            // `idx < n` where n is either `vec.len()` (resolved here) or a
            // local binding to one (resolved via `len_alias`).
            BinOp::Lt => {
                if let ExprKind::Identifier(idx) = &left.kind {
                    let vec_var = self.resolve_len_origin(right)?;
                    return Some(AssertedIndexBound::UpperBound {
                        idx_var: idx.clone(),
                        vec_var,
                    });
                }
                None
            }
            // `n > idx` — same fact as `idx < n`.
            BinOp::Gt => {
                if let ExprKind::Identifier(idx) = &right.kind {
                    let vec_var = self.resolve_len_origin(left)?;
                    return Some(AssertedIndexBound::UpperBound {
                        idx_var: idx.clone(),
                        vec_var,
                    });
                }
                None
            }
            _ => None,
        }
    }

    /// Resolve an expression to the Vec / Slice variable whose `.len()`
    /// it computes, if any. Handles:
    ///   - Direct `coll.len()` method call (Identifier receiver, either
    ///     a Vec or a Slice).
    ///   - A bare Identifier whose binding was previously recorded in
    ///     `len_alias` by the let-site tracking pass (which also covers
    ///     both Vec and Slice receivers).
    pub(super) fn resolve_len_origin(&self, expr: &Expr) -> Option<String> {
        match &expr.kind {
            ExprKind::MethodCall {
                object,
                method,
                args,
                ..
            } if method == "len" && args.is_empty() => {
                if let ExprKind::Identifier(coll_name) = &object.kind {
                    if self.vec_elem_types.contains_key(coll_name.as_str())
                        || self.slice_elem_types.contains_key(coll_name.as_str())
                    {
                        return Some(coll_name.clone());
                    }
                }
                None
            }
            ExprKind::Identifier(name) => self.len_alias.get(name.as_str()).cloned(),
            _ => None,
        }
    }

    pub(super) fn compile_loop(
        &mut self,
        label: Option<&str>,
        body: &Block,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let fn_val = self.current_fn.unwrap();
        let loop_bb = self.context.append_basic_block(fn_val, "loop.body");
        let exit_bb = self.context.append_basic_block(fn_val, "loop.exit");

        // Allocate a slot for `break value` (i64 by default; refined if used)
        let result_slot =
            self.create_entry_alloca(fn_val, "loop.result", self.context.i64_type().into());

        self.loop_stack.push(LoopFrame {
            label: label.map(str::to_string),
            continue_bb: loop_bb,
            break_bb: exit_bb,
            result_slot: Some(result_slot),
        });

        self.builder.build_unconditional_branch(loop_bb).unwrap();
        self.builder.position_at_end(loop_bb);
        self.compile_block(body)?;
        if self
            .builder
            .get_insert_block()
            .unwrap()
            .get_terminator()
            .is_none()
        {
            self.builder.build_unconditional_branch(loop_bb).unwrap();
        }

        self.loop_stack.pop();
        self.builder.position_at_end(exit_bb);
        // Load result (may be zero if no break-with-value was hit)
        let result = self
            .builder
            .build_load::<BasicTypeEnum<'ctx>>(
                self.context.i64_type().into(),
                result_slot,
                "loop.val",
            )
            .unwrap();
        Ok(result)
    }

    /// Compile `label: { body }` (`ExprKind::LabeledBlock`).
    ///
    /// LBC2 / LBC3: allocate an i64 result slot at the entry BB, push a
    /// `LoopFrame` carrying the label and the slot, compile the body,
    /// store the body's tail value (when control falls through normally)
    /// into the slot, branch to a freshly-created `lblock.exit` BB, and
    /// load the slot at the exit. Any `break label expr` inside the body
    /// goes through `compile_break`'s label-aware lookup, stores its
    /// value into the same slot, and branches to the same exit BB.
    ///
    /// Slot LLVM type: i64 today, matching `compile_loop`'s precedent.
    /// The typechecker's LUB constraint already guarantees that for
    /// non-i64-shaped block types, all break sites carry a value of the
    /// same shape — when v1 codegen extends to non-i64 break payloads
    /// (consume `expr_types` lookup), this function and `compile_loop`
    /// flip together. For unit-typed blocks LBC3 specifies the slot is
    /// i64 and `break label` (no value) stores zero.
    ///
    /// `continue_bb` for the frame is a dead `lblock.continue.unreachable`
    /// BB: the resolver rejects `continue label` referring to a labeled
    /// block (`E_CONTINUE_LABEL_BLOCK`), so the BB is never reached at
    /// runtime; pre-allocating it keeps the `LoopFrame` shape uniform.
    pub(super) fn compile_labeled_block(
        &mut self,
        label: &str,
        body: &Block,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let fn_val = self.current_fn.unwrap();
        let i64_t = self.context.i64_type();

        let result_slot = self.create_entry_alloca(fn_val, "lblock.result", i64_t.into());
        // Defense-in-depth zero-init so a never-stored slot loads as 0
        // (matching the unit-equivalent semantics for control paths the
        // typechecker rules out but which a future divergence wouldn't
        // catch).
        self.builder
            .build_store(result_slot, i64_t.const_int(0, false))
            .unwrap();

        let body_bb = self
            .context
            .append_basic_block(fn_val, &format!("lblock.{}.body", label));
        let exit_bb = self
            .context
            .append_basic_block(fn_val, &format!("lblock.{}.exit", label));
        let continue_unreachable_bb = self
            .context
            .append_basic_block(fn_val, &format!("lblock.{}.continue.unreachable", label));

        // Populate the unreachable BB once; it will never branch in.
        // Position back at the previous insert point afterwards.
        let prev_bb = self.builder.get_insert_block();
        self.builder.position_at_end(continue_unreachable_bb);
        self.builder.build_unreachable().unwrap();
        if let Some(bb) = prev_bb {
            self.builder.position_at_end(bb);
        }

        self.builder.build_unconditional_branch(body_bb).unwrap();
        self.builder.position_at_end(body_bb);

        self.loop_stack.push(LoopFrame {
            label: Some(label.to_string()),
            continue_bb: continue_unreachable_bb,
            break_bb: exit_bb,
            result_slot: Some(result_slot),
        });

        // Compile the body. `compile_block` returns the tail expression's
        // value when the block has one; on normal fall-through we store
        // that value into the slot and branch to exit. If the body
        // already terminated (e.g., the tail was an early `break label`,
        // a `return`, or a `panic`), don't add a fall-through branch.
        let tail = self.compile_block(body)?;
        if self
            .builder
            .get_insert_block()
            .unwrap()
            .get_terminator()
            .is_none()
        {
            if let Some(v) = tail {
                if v.is_int_value() {
                    self.builder.build_store(result_slot, v).unwrap();
                }
            }
            self.builder.build_unconditional_branch(exit_bb).unwrap();
        }

        self.loop_stack.pop();
        self.builder.position_at_end(exit_bb);
        let result = self
            .builder
            .build_load::<BasicTypeEnum<'ctx>>(i64_t.into(), result_slot, "lblock.val")
            .unwrap();
        Ok(result)
    }

    pub(super) fn compile_break(
        &mut self,
        label: Option<&str>,
        value: Option<&Expr>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let zero = self.context.i64_type().const_int(0, false);
        // LBC1: label-aware lookup. With `Some(l)`, walk the frame stack
        // top-down and pick the first frame whose label matches; with
        // `None`, fall back to the innermost frame. This is what makes
        // `break outer;` actually skip past `inner` when `outer` is the
        // labeled loop / labeled block (today's pre-slice behavior would
        // always pick the innermost — silent miscompile under nested
        // labels, no test fixture exercised it before this slice).
        let frame = match label {
            Some(l) => self
                .loop_stack
                .iter()
                .rev()
                .find(|f| f.label.as_deref() == Some(l))
                .cloned(),
            None => self.loop_stack.last().cloned(),
        };
        if let Some(frame) = frame {
            if let Some(slot) = frame.result_slot {
                let val = if let Some(v) = value {
                    self.compile_expr(v)?
                } else {
                    zero.into()
                };
                // Store break value (only works when types match i64)
                if val.is_int_value() {
                    self.builder.build_store(slot, val).unwrap();
                }
            }
            self.builder
                .build_unconditional_branch(frame.break_bb)
                .unwrap();
        }
        Ok(zero.into())
    }

    pub(super) fn compile_continue(
        &mut self,
        label: Option<&str>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let zero = self.context.i64_type().const_int(0, false);
        // LBC1: same label-aware lookup as `compile_break`. The resolver
        // guarantees `continue label` only resolves to a `Loop`-kind
        // frame, but the codegen-side dispatch is uniform.
        let frame = match label {
            Some(l) => self
                .loop_stack
                .iter()
                .rev()
                .find(|f| f.label.as_deref() == Some(l))
                .cloned(),
            None => self.loop_stack.last().cloned(),
        };
        if let Some(frame) = frame {
            self.builder
                .build_unconditional_branch(frame.continue_bb)
                .unwrap();
        }
        Ok(zero.into())
    }

    // ── Slice / array patterns (phase-5 § Slice and array patterns sub-item 4)

    /// Resolve a slice-pattern scrutinee expression to a uniform
    /// `SliceSource` — a `T*` data pointer + runtime length + element
    /// type. Handles three identifier-rooted source shapes:
    ///   - `Array[T, N]` (alloca of `[N x T]`) → GEP to elem 0 + const length
    ///   - `Slice[T]` / `mut Slice[T]` (alloca of `{ptr, i64}`) → load data + len
    ///   - `Vec[T]` (alloca of `{ptr, i64, i64}`) → load data + len
    ///
    /// Returns `None` for non-identifier scrutinees or untracked variables —
    /// the typechecker rejects slice patterns against non-sequence
    /// scrutinees, so this is a defensive fallback.
    pub(super) fn resolve_slice_source(&mut self, expr: &Expr) -> Option<SliceSource<'ctx>> {
        let ExprKind::Identifier(name) = &expr.kind else {
            return None;
        };
        let i64_t = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());

        // Array source — alloca holds a [N x T] aggregate. Element pointer is
        // the alloca itself viewed as T* (GEP [0, 0]); length is the static N.
        if let Some(slot) = self.variables.get(name.as_str()).copied() {
            if let BasicTypeEnum::ArrayType(at) = slot.ty {
                let zero = i64_t.const_int(0, false);
                let data_ptr = unsafe {
                    self.builder
                        .build_gep(slot.ty, slot.ptr, &[zero, zero], "sp.ar.data")
                        .unwrap()
                };
                return Some(SliceSource {
                    data_ptr,
                    len: i64_t.const_int(at.len() as u64, false),
                    elem_ty: at.get_element_type(),
                    mutable: false,
                });
            }
            // Slice[T] source.
            if let Some(&elem_ty) = self.slice_elem_types.get(name.as_str()) {
                let slice_ty = self.slice_struct_type();
                let data_pp = self
                    .builder
                    .build_struct_gep(slice_ty, slot.ptr, 0, "sp.sl.dpp")
                    .unwrap();
                let data_ptr = self
                    .builder
                    .build_load(ptr_ty, data_pp, "sp.sl.data")
                    .unwrap()
                    .into_pointer_value();
                let len_p = self
                    .builder
                    .build_struct_gep(slice_ty, slot.ptr, 1, "sp.sl.lp")
                    .unwrap();
                let len = self
                    .builder
                    .build_load(i64_t, len_p, "sp.sl.len")
                    .unwrap()
                    .into_int_value();
                return Some(SliceSource {
                    data_ptr,
                    len,
                    elem_ty,
                    mutable: false,
                });
            }
            // Vec[T] source.
            if let Some(&elem_ty) = self.vec_elem_types.get(name.as_str()) {
                let vec_ty = self.vec_struct_type();
                let data_pp = self
                    .builder
                    .build_struct_gep(vec_ty, slot.ptr, 0, "sp.v.dpp")
                    .unwrap();
                let data_ptr = self
                    .builder
                    .build_load(ptr_ty, data_pp, "sp.v.data")
                    .unwrap()
                    .into_pointer_value();
                let len_p = self
                    .builder
                    .build_struct_gep(vec_ty, slot.ptr, 1, "sp.v.lp")
                    .unwrap();
                let len = self
                    .builder
                    .build_load(i64_t, len_p, "sp.v.len")
                    .unwrap()
                    .into_int_value();
                return Some(SliceSource {
                    data_ptr,
                    len,
                    elem_ty,
                    mutable: false,
                });
            }
        }
        None
    }

    /// Load element `T` at `idx` from a slice source — GEP with the element
    /// type then load.
    pub(super) fn load_slice_pattern_element(
        &self,
        src: &SliceSource<'ctx>,
        idx: IntValue<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        let elem_ptr = unsafe {
            self.builder
                .build_gep(src.elem_ty, src.data_ptr, &[idx], "sp.elem.ptr")
                .unwrap()
        };
        self.builder
            .build_load(src.elem_ty, elem_ptr, "sp.elem")
            .unwrap()
    }

    /// Compile the i1 condition for `[prefix..., rest?, suffix...]` against
    /// a `SliceSource`. The length check fires first; sub-pattern checks
    /// run only when the length passes (guarded via a "check_elems" block
    /// so OOB GEPs don't emit when the length is wrong). Returns a phi-ed
    /// i1 that is false on length-mismatch and the AND of sub-pattern
    /// conditions otherwise.
    pub(super) fn compile_slice_pattern_condition(
        &mut self,
        prefix: &[Pattern],
        rest: &Option<RestPattern>,
        suffix: &[Pattern],
        src: &SliceSource<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let i64_t = self.context.i64_type();
        let bool_t = self.context.bool_type();
        let fn_val = self.current_fn.unwrap();

        let min_len = i64_t.const_int((prefix.len() + suffix.len()) as u64, false);
        let len_ok = if rest.is_none() {
            self.builder
                .build_int_compare(IntPredicate::EQ, src.len, min_len, "sp.len.eq")
                .unwrap()
        } else {
            self.builder
                .build_int_compare(IntPredicate::UGE, src.len, min_len, "sp.len.ge")
                .unwrap()
        };

        // Fast path when there are no sub-patterns to check: condition is
        // just the length test.
        if prefix.is_empty() && suffix.is_empty() {
            return Ok(len_ok.into());
        }

        let check_bb = self.context.append_basic_block(fn_val, "sp.check");
        let done_bb = self.context.append_basic_block(fn_val, "sp.done");
        let len_fail_bb = self.builder.get_insert_block().unwrap();
        self.builder
            .build_conditional_branch(len_ok, check_bb, done_bb)
            .unwrap();

        self.builder.position_at_end(check_bb);
        let mut cond: IntValue<'ctx> = bool_t.const_int(1, false);
        for (i, sub) in prefix.iter().enumerate() {
            let idx = i64_t.const_int(i as u64, false);
            let elem = self.load_slice_pattern_element(src, idx);
            let sub_cond = self.compile_pattern_condition(sub, elem)?.into_int_value();
            cond = self.builder.build_and(cond, sub_cond, "sp.and").unwrap();
        }
        for (i, sub) in suffix.iter().enumerate() {
            let back_off = i64_t.const_int((suffix.len() - i) as u64, false);
            let idx = self
                .builder
                .build_int_sub(src.len, back_off, "sp.suf.idx")
                .unwrap();
            let elem = self.load_slice_pattern_element(src, idx);
            let sub_cond = self.compile_pattern_condition(sub, elem)?.into_int_value();
            cond = self.builder.build_and(cond, sub_cond, "sp.and").unwrap();
        }
        let check_end = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(done_bb).unwrap();

        self.builder.position_at_end(done_bb);
        let phi = self.builder.build_phi(bool_t, "sp.cond").unwrap();
        let len_false = bool_t.const_int(0, false);
        phi.add_incoming(&[(&len_false, len_fail_bb), (&cond, check_end)]);
        Ok(phi.as_basic_value())
    }

    /// Bind sub-patterns of a slice pattern against the source. Prefix
    /// elements bind at `data_ptr[i]`, suffix at `data_ptr[len-j+i]`,
    /// and `RestPattern::Bound(name)` materializes a `Slice[T]` view
    /// over `data_ptr[k..len-j]` registered under `name` so user code
    /// can dispatch slice methods (`rest.len()`, `rest[0]`, etc.).
    /// `for_match` toggles the sub-pattern binder between the match-arm
    /// helper (`bind_pattern_values`) and the let helper (`bind_pattern`),
    /// matching the two call sites' surrounding semantics.
    pub(super) fn bind_slice_pattern(
        &mut self,
        prefix: &[Pattern],
        rest: &Option<RestPattern>,
        suffix: &[Pattern],
        src: &SliceSource<'ctx>,
        for_match: bool,
    ) -> Result<(), String> {
        let i64_t = self.context.i64_type();

        for (i, sub) in prefix.iter().enumerate() {
            let idx = i64_t.const_int(i as u64, false);
            let elem = self.load_slice_pattern_element(src, idx);
            if for_match {
                self.bind_pattern_values(sub, elem)?;
            } else {
                self.bind_pattern(sub, elem)?;
            }
        }
        for (i, sub) in suffix.iter().enumerate() {
            let back_off = i64_t.const_int((suffix.len() - i) as u64, false);
            let idx = self
                .builder
                .build_int_sub(src.len, back_off, "sp.suf.bind.idx")
                .unwrap();
            let elem = self.load_slice_pattern_element(src, idx);
            if for_match {
                self.bind_pattern_values(sub, elem)?;
            } else {
                self.bind_pattern(sub, elem)?;
            }
        }

        if let Some(RestPattern::Bound(name)) = rest {
            let fn_val = self.current_fn.unwrap();
            let slice_ty = self.slice_struct_type();
            let prefix_off = i64_t.const_int(prefix.len() as u64, false);
            let suffix_len = i64_t.const_int(suffix.len() as u64, false);
            let rest_data_ptr = unsafe {
                self.builder
                    .build_gep(src.elem_ty, src.data_ptr, &[prefix_off], "sp.rest.dp")
                    .unwrap()
            };
            let after_prefix = self
                .builder
                .build_int_sub(src.len, prefix_off, "sp.rest.lp1")
                .unwrap();
            let rest_len = self
                .builder
                .build_int_sub(after_prefix, suffix_len, "sp.rest.len")
                .unwrap();
            let slice_val = self.build_slice_header(slice_ty, rest_data_ptr, rest_len);
            let alloca = self.create_entry_alloca(fn_val, name, slice_ty.into());
            self.builder.build_store(alloca, slice_val).unwrap();
            self.variables.insert(
                name.clone(),
                VarSlot {
                    ptr: alloca,
                    ty: slice_ty.into(),
                },
            );
            self.slice_elem_types.insert(name.clone(), src.elem_ty);
        }
        // `mutable` is a typechecker-level concept — codegen layout is
        // identical for read-only and mut slices; ownership tracking is
        // handled separately.
        let _ = src.mutable;
        Ok(())
    }
}
