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

use inkwell::basic_block::BasicBlock;
use inkwell::types::{BasicTypeEnum, IntType};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, IntValue};

use super::state::LoopFrame;

impl<'ctx> super::Codegen<'ctx> {
    // ── IfLet ────────────────────────────────────────────────────

    pub(super) fn compile_if_let(
        &mut self,
        pattern: &Pattern,
        value: &Expr,
        then_block: &Block,
        else_branch: Option<&Expr>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        // Tail-return context: consume it now (the scrutinee `value` below is
        // NOT a tail return), then re-arm it for each branch's final expr so
        // a bare-arg `Option[shared]` leaf gets its per-branch inc.
        let tail = self.tail_ret_inner.take();
        let val = self.compile_expr(value)?;
        // B-track (pattern-arm unbound heap-field drop): a fresh-temp enum
        // scrutinee with a heap-bearing payload has no source `EnumDrop`, so an
        // arm that leaves a heap field unbound leaks it (and the miss edge
        // leaks the whole temp). Materialize + `track_enum_var` here so the
        // enum's drop walk frees the unbound fields at the enclosing scope's
        // exit; the suppression after `bind_pattern_values` (then-arm only)
        // zeroes the caps of fields the pattern moved into bindings. No-op for
        // non-fresh / non-enum scrutinees.
        let freshtemp_enum = self.materialize_freshtemp_enum_scrutinee(value, pattern, val, false);
        // Oversized-enum-payload §1/§2: free the heap box for a fresh-temp
        // Option[Wide]/Result[Wide,_] scrutinee (box-only — the bound payload
        // owns its inner heap). Registers in the enclosing frame, so the box
        // frees on both the match and miss edges.
        if freshtemp_enum.is_none() {
            self.track_freshtemp_boxed_enum_scrutinee(value, &[pattern], val);
        }
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
        // The cleanup frame is pushed BEFORE the pattern bind (mirroring
        // `compile_while_let`'s body frame and match arms) so a shared
        // pattern binding's scope-exit `RcDec` (`bind_pattern_values`'
        // alias acquire) drains at the END OF THIS ARM — not in the
        // enclosing frame, where a then-block inside a loop would inc once
        // per iteration but dec only once at the enclosing scope's exit.
        self.scope_cleanup_actions.push(Vec::new());
        self.bind_pattern_values(pattern, val)?;
        // B-track: zero the caps of moved-in fields so the source EnumDrop
        // (registered above) frees only the *unbound* heap fields, not the ones
        // the pattern's bindings now own. Then-arm only — the else/miss edge
        // runs no suppression so the drop walk frees the temp wholesale.
        if let Some((alloca, enum_name)) = &freshtemp_enum {
            self.suppress_destructured_enum_payload_cleanup_at(*alloca, enum_name, pattern);
        }
        // B-2026-06-10-6: a variable `Option[String]`/`Option[Vec]` scrutinee
        // with a `FreeInlineOptionPayload` needs its source `cap` zeroed when
        // this arm binds the payload out, else x's scope-exit free doubles
        // the binding's. No-op for temp / non-inline scrutinees.
        self.suppress_inline_option_payload_cleanup(value, pattern);
        self.suppress_inline_result_payload_cleanup(value, pattern);
        self.suppress_inline_option_map_payload_cleanup(value, pattern);
        self.tail_ret_inner = tail;
        let then_val = self.compile_block(then_block)?;
        let then_terminated = self
            .builder
            .get_insert_block()
            .unwrap()
            .get_terminator()
            .is_some();
        if !then_terminated {
            self.drain_top_frame_with_emit();
        } else {
            self.scope_cleanup_actions.pop();
        }
        let then_end = self.builder.get_insert_block().unwrap();
        if !then_terminated {
            self.builder.build_unconditional_branch(merge_bb).unwrap();
        }

        self.builder.position_at_end(else_bb);
        let else_val = if let Some(eb) = else_branch {
            self.tail_ret_inner = tail;
            match &eb.kind {
                ExprKind::Block(blk) => self.compile_block_with_frame(blk)?,
                _ => Some(self.compile_expr(eb)?),
            }
        } else {
            None
        };
        self.tail_ret_inner = None;
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
        let placeholder = self.context.i64_type().const_int(0, false).into();
        match (then_terminated, else_terminated) {
            // Both arms diverge — terminate the unreachable merge block (see
            // `compile_if` for the gap-d rationale).
            (true, true) => {
                self.builder.build_unreachable().unwrap();
                Ok(placeholder)
            }
            // Exactly one arm diverges: the `if let` value is the live arm's
            // value (single live predecessor dominates the merge).
            (true, false) => Ok(else_val.unwrap_or(placeholder)),
            (false, true) => Ok(then_val.unwrap_or(placeholder)),
            (false, false) => {
                if let (Some(tv), Some(ev)) = (then_val, else_val) {
                    // Same narrow-int width reconciliation as `compile_if`.
                    let (tv, ev) = self.unify_int_branch_widths(tv, then_end, ev, else_end);
                    if tv.get_type() == ev.get_type() {
                        let phi = self.builder.build_phi(tv.get_type(), "ifletval").unwrap();
                        phi.add_incoming(&[(&tv, then_end), (&ev, else_end)]);
                        return Ok(phi.as_basic_value());
                    }
                }
                Ok(placeholder)
            }
        }
    }

    // ── WhileLet ─────────────────────────────────────────────────

    /// Lower `while let PAT = SCRUT { BODY }` (phase-6-runtime.md line 489).
    /// Structurally a `compile_while` whose condition is a pattern test:
    /// the loop header re-evaluates the scrutinee each iteration, tests it
    /// against the pattern (`compile_pattern_condition`), and on a match
    /// binds the pattern's names (`bind_pattern_values`) before running the
    /// body. A per-iteration scope-cleanup frame (same shape as
    /// `compile_while`) drops the iteration's pattern bindings and any body
    /// temporaries before the next iteration's scrutinee is evaluated.
    pub(super) fn compile_while_let(
        &mut self,
        label: Option<&str>,
        pattern: &Pattern,
        value: &Expr,
        body: &Block,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let fn_val = self.current_fn.unwrap();
        let cond_bb = self.context.append_basic_block(fn_val, "whilelet.cond");
        let body_bb = self.context.append_basic_block(fn_val, "whilelet.body");
        // The miss edge gets its own block (rather than branching straight to
        // exit) so the final non-matching fresh-temp scrutinee can be dropped
        // there — see the loop-exit handling below.
        let miss_bb = self.context.append_basic_block(fn_val, "whilelet.miss");
        let exit_bb = self.context.append_basic_block(fn_val, "whilelet.exit");

        self.builder.build_unconditional_branch(cond_bb).unwrap();

        self.loop_stack.push(LoopFrame {
            label: label.map(str::to_string),
            continue_bb: cond_bb,
            break_bb: exit_bb,
            result_slot: None,
            cleanup_depth: self.scope_cleanup_actions.len(),
        });

        // Header: re-evaluate the scrutinee and test the pattern every
        // iteration. `val` is defined in `cond_bb`, which dominates
        // `body_bb`, so the bind below can reuse it (same SSA shape as
        // `compile_if_let`).
        self.builder.position_at_end(cond_bb);
        let val = self.compile_expr(value)?;
        let cond = self.compile_pattern_condition(pattern, val)?;
        self.builder
            .build_conditional_branch(cond.into_int_value(), body_bb, miss_bb)
            .unwrap();

        self.builder.position_at_end(body_bb);
        // Per-iteration scope frame, same shape as `compile_while` — see its
        // comment for the leak rationale.
        self.scope_cleanup_actions.push(Vec::new());
        // B-track (pattern-arm unbound heap-field drop): a fresh-temp enum
        // scrutinee with a heap payload field the arm leaves unbound leaks per
        // iteration. Unlike if-let/let-else (one enclosing-frame drop), the
        // materialize + `track_enum_var` must register in the *per-iteration*
        // body frame (pushed just above) so the EnumDrop drains at the bottom
        // of each iteration and the entry alloca is overwritten by the next
        // iteration's scrutinee before being read again. The store emits here
        // in `body_bb` (dominated by `cond_bb` where `val` is defined). The
        // heap-bearing *miss* variant at loop exit (the final non-matching
        // scrutinee) is freed wholesale on the `miss_bb` edge below.
        let freshtemp_enum = self.materialize_freshtemp_enum_scrutinee(value, pattern, val, false);
        // Oversized-enum-payload §1/§2: free the heap box for a fresh-temp
        // boxed-payload scrutinee, registered in the per-iteration body frame
        // (drains each iteration). An `Option` loop terminates on `None` (no
        // box), so no miss-edge box free is needed; a `Result`-terminating
        // boxed `Err` miss is deferred (spike §1, rare shape).
        if freshtemp_enum.is_none() {
            self.track_freshtemp_boxed_enum_scrutinee(value, &[pattern], val);
        }
        self.bind_pattern_values(pattern, val)?;
        if let Some((alloca, enum_name)) = &freshtemp_enum {
            self.suppress_destructured_enum_payload_cleanup_at(*alloca, enum_name, pattern);
        }
        // B-2026-06-10-6: variable inline-`Option` scrutinee source-cap
        // suppression (see `compile_if_let`). No-op for temp / non-inline.
        self.suppress_inline_option_payload_cleanup(value, pattern);
        self.suppress_inline_result_payload_cleanup(value, pattern);
        self.suppress_inline_option_map_payload_cleanup(value, pattern);
        self.compile_block(body)?;
        let body_has_terminator = self
            .builder
            .get_insert_block()
            .unwrap()
            .get_terminator()
            .is_some();
        if !body_has_terminator {
            self.drain_top_frame_with_emit();
            self.builder.build_unconditional_branch(cond_bb).unwrap();
        } else {
            self.scope_cleanup_actions.pop();
        }

        self.loop_stack.pop();

        // Miss edge (loop exit): the final scrutinee did not match the
        // pattern. If it is a fresh-temp enum carrying heap in its (unmatched)
        // variant, free it wholesale here — it never entered the per-iteration
        // body frame, so this is the only place it can be dropped (B
        // follow-up #2). A miss binds nothing out, so no cap-suppression: the
        // whole value drops. `val` is defined in `cond_bb`, which dominates
        // `miss_bb`. Place / heap-free scrutinees are a no-op (the helper's
        // gate), so a place scrutinee keeps its owner's cleanup untouched.
        self.builder.position_at_end(miss_bb);
        self.drop_freshtemp_enum_scrutinee_on_miss(value, pattern, val);
        self.builder.build_unconditional_branch(exit_bb).unwrap();

        self.builder.position_at_end(exit_bb);
        Ok(self.context.i64_type().const_int(0, false).into())
    }

    // ── LetElse ──────────────────────────────────────────────────

    /// Lower `let PAT = SCRUT else { ELSE }` (phase-6-runtime.md line 489).
    /// Evaluate the scrutinee, test it against the pattern, and branch: on a
    /// match, bind the pattern's names into the **enclosing** scope (so they
    /// are live for the rest of the block) and fall through to the following
    /// statements; on a miss, run the else block, which the typechecker has
    /// already verified diverges (`return` / `break` / `continue` / panic).
    /// Mirrors `compile_if_let`'s scrutinee+condition machinery, but the
    /// bindings escape the construct and there is no merge block — the match
    /// edge continues straight into the block and the else edge diverges.
    pub(super) fn compile_let_else(
        &mut self,
        pattern: &Pattern,
        value: &Expr,
        else_block: &Block,
    ) -> Result<(), String> {
        let val = self.compile_expr(value)?;
        // B-track (pattern-arm unbound heap-field drop): same fresh-temp enum
        // scrutinee fix as `compile_if_let`. The `EnumDrop` registered here
        // drains at the enclosing scope's exit on the match edge (after the
        // escaped bindings), and at the divergent else edge's
        // `emit_scope_cleanup` walk on the miss edge (wholesale). Suppression
        // on the match edge zeroes the caps of moved-in fields.
        let freshtemp_enum = self.materialize_freshtemp_enum_scrutinee(value, pattern, val, false);
        // Oversized-enum-payload §1/§2: free the heap box for a fresh-temp
        // boxed-payload scrutinee (box-only). Registers in the enclosing frame,
        // so it frees after the escaped bindings on the match edge and via the
        // divergent else edge's cleanup walk on the miss edge.
        if freshtemp_enum.is_none() {
            self.track_freshtemp_boxed_enum_scrutinee(value, &[pattern], val);
        }
        let cond = self.compile_pattern_condition(pattern, val)?;

        let fn_val = self.current_fn.unwrap();
        let match_bb = self.context.append_basic_block(fn_val, "letelse.match");
        let else_bb = self.context.append_basic_block(fn_val, "letelse.else");

        self.builder
            .build_conditional_branch(cond.into_int_value(), match_bb, else_bb)
            .unwrap();

        // Else edge: the block diverges (typecheck-enforced). Compile it in
        // its own scope frame; the divergent exit's `emit_scope_cleanup`
        // walks that frame. Guard against a missing terminator defensively —
        // a well-typed program always terminates here.
        self.builder.position_at_end(else_bb);
        self.compile_block_with_frame(else_block)?;
        if self
            .builder
            .get_insert_block()
            .unwrap()
            .get_terminator()
            .is_none()
        {
            self.builder.build_unreachable().unwrap();
        }

        // Match edge: bind into the current (enclosing) scope and fall
        // through. `val` is defined before the branch and dominates here.
        self.builder.position_at_end(match_bb);
        self.bind_pattern_values(pattern, val)?;
        if let Some((alloca, enum_name)) = &freshtemp_enum {
            self.suppress_destructured_enum_payload_cleanup_at(*alloca, enum_name, pattern);
        }
        // B-2026-06-10-6: variable inline-`Option` scrutinee — `s` binds into
        // the enclosing scope where x's `FreeInlineOptionPayload` also lives,
        // so zero x's source `cap` to avoid a double-free at that scope's exit.
        self.suppress_inline_option_payload_cleanup(value, pattern);
        self.suppress_inline_result_payload_cleanup(value, pattern);
        self.suppress_inline_option_map_payload_cleanup(value, pattern);
        Ok(())
    }

    /// The libc `FILE*` for stdout (`to_stderr == false`) or stderr, as the
    /// `fwrite` stream argument. On glibc / wasi-libc / Apple this loads the
    /// `stdout` / `stderr` (`__stdoutp` / `__stderrp`) data global. The MSVC
    /// UCRT exposes **no such data symbol** — `<stdio.h>`'s `stdout` / `stderr`
    /// are macros over `__acrt_iob_func(n)` (1 = stdout, 2 = stderr) — so a
    /// Windows build emits that call instead. Without it the linked object
    /// carries an undefined `stdout` reference (`lld-link: error: undefined
    /// symbol: stdout`). Host-`cfg`'d, mirroring the `__stdoutp` Apple branch in
    /// `Codegen::new` — karac is built natively per target. Both arms are
    /// syntactically live so `stdout_global` is never "field never read" on
    /// Windows.
    fn stdio_stream(&self, to_stderr: bool) -> inkwell::values::BasicValueEnum<'ctx> {
        let ptr_t = self.context.ptr_type(inkwell::AddressSpace::default());
        if cfg!(windows) {
            let i32_t = self.context.i32_type();
            let iob = self
                .module
                .get_function("__acrt_iob_func")
                .unwrap_or_else(|| {
                    self.module.add_function(
                        "__acrt_iob_func",
                        ptr_t.fn_type(&[i32_t.into()], false),
                        Some(inkwell::module::Linkage::External),
                    )
                });
            let idx = i32_t.const_int(if to_stderr { 2 } else { 1 }, false);
            self.builder
                .build_call(iob, &[idx.into()], "iob")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
        } else {
            let glob = if to_stderr {
                self.stderr_global
            } else {
                self.stdout_global
            };
            self.builder
                .build_load(ptr_t, glob.as_pointer_value(), "fw.stream")
                .unwrap()
        }
    }

    /// Write exactly `len` bytes of `data` to stdout (or stderr) via `fwrite`,
    /// followed by the newline `nl`. NUL-safe — unlike `printf("%.*s")`, which
    /// stops at the first interior NUL even with a precision set (so a
    /// length-prefixed String carrying `\0` would print truncated, L5).
    /// `fwrite` shares libc's stdio buffer with the `printf` int/bool print
    /// paths, so output ordering across mixed prints is preserved.
    pub(super) fn emit_nul_safe_write(
        &mut self,
        data: inkwell::values::PointerValue<'ctx>,
        len: inkwell::values::IntValue<'ctx>,
        nl: &str,
        to_stderr: bool,
    ) {
        // `fwrite`'s size args are `size_t` — i32 on wasm32, i64 natively
        // (must match the extern declaration EXACTLY; wasm traps a mismatch).
        let size_t = if crate::target::active_target_is_wasm() {
            self.context.i32_type()
        } else {
            self.context.i64_type()
        };
        // Normalize the byte length to size_t (truncate a wider i64 on wasm,
        // widen a narrower count like the char-codepoint path's byte count).
        let len_st = {
            let cur = len.get_type().get_bit_width();
            let want = size_t.get_bit_width();
            if cur == want {
                len
            } else if cur > want {
                self.builder
                    .build_int_truncate(len, size_t, "fw.len.st")
                    .unwrap()
            } else {
                self.builder
                    .build_int_z_extend(len, size_t, "fw.len.st")
                    .unwrap()
            }
        };
        let stream = self.stdio_stream(to_stderr);
        // Route through the runtime console chokepoint (auto-par ordered-
        // output): at the top level it `fwrite`s `len` bytes to `stream` (the
        // old inline behavior); inside a parallel branch it captures the bytes
        // for ordered replay at the join. `write_console` folds in the `1`
        // element-size, so only (data, len, stream) cross the call boundary.
        self.builder
            .build_call(
                self.write_console_fn,
                &[
                    BasicMetadataValueEnum::from(data),
                    BasicMetadataValueEnum::from(len_st),
                    BasicMetadataValueEnum::from(stream),
                ],
                "wc",
            )
            .unwrap();
        if !nl.is_empty() {
            let nl_g = self.builder.build_global_string_ptr(nl, "fw.nl").unwrap();
            let nl_len = size_t.const_int(nl.len() as u64, false);
            let stream2 = self.stdio_stream(to_stderr);
            self.builder
                .build_call(
                    self.write_console_fn,
                    &[
                        BasicMetadataValueEnum::from(nl_g.as_pointer_value()),
                        BasicMetadataValueEnum::from(nl_len),
                        BasicMetadataValueEnum::from(stream2),
                    ],
                    "wc.nl.call",
                )
                .unwrap();
        }
    }

    /// Print a String value (`{data,len,cap}`) NUL-safely + the newline `nl`,
    /// then free its heap buffer. Used by the collection-Display print arms,
    /// which render into a throwaway accumulator and must release it inline
    /// (no scope-tracking — avoids per-call buffer accumulation in loops).
    fn emit_print_and_free_string(&mut self, sval: BasicValueEnum<'ctx>, nl: &str) {
        self.emit_write_and_free_string(sval, nl, false);
    }

    /// `emit_print_and_free_string` with an explicit stream selector: write the
    /// owning String value to stdout (`to_stderr == false`) or stderr
    /// (`true`), append `nl`, then free its heap buffer. The stderr arm backs
    /// the `main() -> Result` `Err(e)` exit, whose `Error: {e}\n` rendering
    /// must land on stderr per design.md § Entry Point (B-2026-06-12-9).
    pub(super) fn emit_write_and_free_string(
        &mut self,
        sval: BasicValueEnum<'ctx>,
        nl: &str,
        to_stderr: bool,
    ) {
        let sv = sval.into_struct_value();
        let data = self
            .builder
            .build_extract_value(sv, 0, "ps.data")
            .unwrap()
            .into_pointer_value();
        let len = self
            .builder
            .build_extract_value(sv, 1, "ps.len")
            .unwrap()
            .into_int_value();
        self.emit_nul_safe_write(data, len, nl, to_stderr);
        // Free only an OWNING String. The invariant (mirrored by the f-string
        // accumulator's scope-exit cleanup) is `cap == 0 ⇔ non-owning` — a
        // literal-backed String points its `data` at a read-only global and
        // carries `cap == 0`. Built-in display renderers always return an owned
        // (`cap > 0`) String, so this guard is a no-op for them; it is
        // load-bearing for a user `impl Display` whose `to_string` returns a
        // string literal (e.g. `match self { Red => "red", … }`), where an
        // unconditional `free` of the global aborts (SIGABRT). GAP-W4.
        let cap = self
            .builder
            .build_extract_value(sv, 2, "ps.cap")
            .unwrap()
            .into_int_value();
        let fn_val = self.current_fn.unwrap();
        let do_free = self.context.append_basic_block(fn_val, "ps.free");
        let after = self.context.append_basic_block(fn_val, "ps.after");
        let owns = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::UGT,
                cap,
                self.context.i64_type().const_zero(),
                "ps.owns",
            )
            .unwrap();
        self.builder
            .build_conditional_branch(owns, do_free, after)
            .unwrap();
        self.builder.position_at_end(do_free);
        self.builder
            .build_call(self.free_fn, &[data.into()], "")
            .unwrap();
        self.builder.build_unconditional_branch(after).unwrap();
        self.builder.position_at_end(after);
    }

    pub(super) fn compile_print(
        &mut self,
        name: &str,
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let zero = self.context.i64_type().const_int(0, false);
        if args.is_empty() {
            // Route through the console chokepoint (not `printf`) so a bare
            // `println()` inside a parallel branch is captured + ordered too.
            let nl_g = self.builder.build_global_string_ptr("\n", "nl").unwrap();
            self.emit_nul_safe_write(
                nl_g.as_pointer_value(),
                self.context.i64_type().const_int(1, false),
                "",
                false,
            );
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
                let (_acc, sval) = self.render_via_display_fn(display_fn, slot.ptr);
                self.emit_print_and_free_string(sval, nl);
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
                let (_acc, sval) = self.render_via_display_fn(display_fn, slot.ptr);
                self.emit_print_and_free_string(sval, nl);
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
                let (_acc, sval) = self.render_via_display_fn(display_fn, slot.ptr);
                self.emit_print_and_free_string(sval, nl);
                return Ok(zero.into());
            }
        }

        // User `impl Display` (a compiled `<Type>.to_string`) wins over every
        // built-in renderer below — render `println(x)` via the user method,
        // matching `f"{x}"` / `x.to_string()` and the interpreter. The owning
        // String it returns is printed + freed. GAP-W4.
        if self.user_display_impl_type(&args[0].value).is_some() {
            let sval =
                self.compile_method_call(&args[0].value, "to_string", &[], &args[0].value.span)?;
            self.emit_print_and_free_string(sval, nl);
            return Ok(zero.into());
        }

        // All-unit enum arm — render the bare variant name (selected on the
        // tag). Precedes the value-kind arms for the same reason as the struct
        // arm below (an enum lowers to a tagged struct value).
        if let Some(ename) = self.expr_user_enum_name(&args[0].value) {
            let (data, len) = self.compile_unit_enum_display(&args[0].value, &ename)?;
            self.emit_nul_safe_write(data, len, nl, false);
            return Ok(zero.into());
        }

        // Payload-bearing user enum arm — render via its value-driven Display
        // fn (`Variant` / `Variant(f0, f1)` / `Variant { name: v }`), then
        // print + free the owning buffer.
        if let Some(ename) = self.expr_user_enum_name_any(&args[0].value) {
            let (_acc, sval) = self.render_user_enum_display(&args[0].value, &ename)?;
            self.emit_print_and_free_string(sval, nl);
            return Ok(zero.into());
        }

        // User-struct arm — `#[derive(Display)]` / `impl Display` structs
        // render as `TypeName { field: value, … }` in declaration order
        // (matching the interpreter). Render to an owning String via the
        // synthetic-f-string path, then print it NUL-safely. Must precede
        // the value-kind arms below: a user struct lowers to a struct value
        // that is NOT the 3-field String layout, so without this it would hit
        // the String / raw-pointer arm and ICE / print an address.
        if let Some(sname) = self.expr_user_struct_name(&args[0].value) {
            let s = self
                .compile_struct_display_string(&args[0].value, &sname)?
                .into_struct_value();
            let data = self
                .builder
                .build_extract_value(s, 0, "pd.data")
                .unwrap()
                .into_pointer_value();
            let len = self
                .builder
                .build_extract_value(s, 1, "pd.len")
                .unwrap()
                .into_int_value();
            self.emit_nul_safe_write(data, len, nl, false);
            return Ok(zero.into());
        }

        // Char arm — render as the UTF-8 glyph rather than the integer
        // codepoint. Must precede the generic int path because `char`
        // lowers to `i32` and would otherwise hit the `%lld` branch.
        // The detection covers literals (`println('A')`), char-typed
        // identifiers (`for c in s.chars() { println(c); }`,
        // `let c: char = 'A'; println(c);`), and Vec/Array indexed
        // reads (`println(chars[i])`).
        if self.expr_is_char(&args[0].value) {
            let val = self.compile_expr(&args[0].value)?;
            let (buf_ptr, byte_len) = self.emit_codepoint_to_utf8(val.into_int_value());
            // NUL-safe write: `'\0'` is a single 0x00 byte (byte_len 1) — a
            // `%.*s` print would emit nothing; `fwrite` emits the NUL (L5).
            self.emit_nul_safe_write(buf_ptr, byte_len, nl, false);
            return Ok(zero.into());
        }

        let val = self.compile_expr(&args[0].value)?;

        if val.is_int_value() {
            let bits = val.into_int_value().get_type().get_bit_width();
            if bits == 1 {
                // Select the literal + its length, then route through the
                // console chokepoint (capture-aware) instead of `printf` — `nl`
                // is appended by `emit_nul_safe_write`, not baked into the text.
                let true_s = self.builder.build_global_string_ptr("true", "ts").unwrap();
                let false_s = self.builder.build_global_string_ptr("false", "fs").unwrap();
                let i64_t = self.context.i64_type();
                let sel_ptr = self
                    .builder
                    .build_select(
                        val.into_int_value(),
                        true_s.as_pointer_value(),
                        false_s.as_pointer_value(),
                        "bstr",
                    )
                    .unwrap()
                    .into_pointer_value();
                let sel_len = self
                    .builder
                    .build_select(
                        val.into_int_value(),
                        i64_t.const_int(4, false),
                        i64_t.const_int(5, false),
                        "blen",
                    )
                    .unwrap()
                    .into_int_value();
                self.emit_nul_safe_write(sel_ptr, sel_len, nl, false);
            } else {
                // Widen narrower ints to i64 before printf's varargs slot —
                // sign-extend for signed types so a negative `i32` prints as
                // a signed decimal, zero-extend for unsigned types so a
                // large `u32` doesn't get sign-mistreated. Pre-fix this arm
                // passed the raw `i32` to `%lld`, which LLVM zero-padded
                // before the call and printf then read as a 64-bit signed
                // value — giving the unsigned-representation print on
                // negative narrow ints (e.g. `i32 -123` → `4294967173`).
                // Mirrors the per-type display dispatch in
                // [`synth_display::emit_primitive_display`].
                let int_val = val.into_int_value();
                let bits = int_val.get_type().get_bit_width();
                let i64_t = self.context.i64_type();
                let is_unsigned = self.expr_is_unsigned_int(&args[0].value);
                let widened = if bits < 64 {
                    if is_unsigned {
                        self.builder
                            .build_int_z_extend(int_val, i64_t, "print.zext")
                            .unwrap()
                    } else {
                        self.builder
                            .build_int_s_extend(int_val, i64_t, "print.sext")
                            .unwrap()
                    }
                } else {
                    int_val
                };
                // Render into a stack buffer via `snprintf`, then route the
                // exact bytes through the console chokepoint (capture-aware)
                // instead of `printf` — so an int `println` inside a parallel
                // branch is captured + flushed in order. 32 bytes covers any
                // i64 (≤20 digits + sign + NUL). `nl` is appended by the write.
                let spec = if is_unsigned { "%llu" } else { "%lld" };
                let fmt = self.builder.build_global_string_ptr(spec, "fi").unwrap();
                let ptr_t = self.context.ptr_type(inkwell::AddressSpace::default());
                let size_t = if crate::target::active_target_is_wasm() {
                    self.context.i32_type()
                } else {
                    self.context.i64_type()
                };
                let fn_val = self.current_fn.unwrap();
                let buf = self.create_entry_alloca(
                    fn_val,
                    "ibuf",
                    self.context.i8_type().array_type(32).into(),
                );
                let buf_ptr = self
                    .builder
                    .build_pointer_cast(buf, ptr_t, "ibufp")
                    .unwrap();
                let written = self
                    .builder
                    .build_call(
                        self.snprintf_fn,
                        &[
                            buf_ptr.into(),
                            size_t.const_int(32, false).into(),
                            fmt.as_pointer_value().into(),
                            widened.into(),
                        ],
                        "iwritten",
                    )
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_int_value();
                self.emit_nul_safe_write(buf_ptr, written, nl, false);
            }
        } else if val.is_struct_value() {
            // A user struct that reached here is a `println(StructLiteral{…})`
            // / `println(make())` argument — the declaration-order struct
            // Display arm above only fires for place-expression args
            // (identifier / field access). Emit a clean error rather than
            // mis-reading the struct as the String `{ptr,i64,i64}` layout
            // below (which would extract a non-pointer field and ICE).
            if !self.llvm_ty_is_vec_struct(val.into_struct_value().get_type().into()) {
                return Err(
                    "Display of a struct argument is supported when the argument is a \
                     variable or field access (e.g. `let x = …; println(x)` / `x.to_string()`); \
                     bind a struct literal or call result to a `let` first (user-struct \
                     Display, subtask-5 follow-on)"
                        .to_string(),
                );
            }
            // String struct `{ ptr, i64, i64 }` (data, len, cap). An earlier
            // fix moved this off a bare `%s` (which puts/printf treats as a
            // NUL-terminated C string and walks past a non-terminated heap
            // buffer — an ASAN 1-byte heap-buffer-overflow) onto `%.*s` with
            // the explicit `len`. But `%.*s` ALSO stops at an interior NUL
            // even with a precision, so a String carrying `\0` printed
            // truncated. `emit_nul_safe_write` lowers to `fwrite`, which
            // writes exactly `len` bytes regardless of NULs (L5) and still
            // never reads past the buffer.
            let sv = val.into_struct_value();
            let str_ptr = self
                .builder
                .build_extract_value(sv, 0, "str.ptr")
                .unwrap()
                .into_pointer_value();
            let str_len = self
                .builder
                .build_extract_value(sv, 1, "str.len")
                .unwrap()
                .into_int_value();
            self.emit_nul_safe_write(str_ptr, str_len, nl, false);
            // #20: a fresh-owned String temp passed directly to `println` /
            // `print` (`println(i.to_string())`, `print(a + b)`) has no
            // consuming binding, so its heap buffer would leak once per call —
            // unbounded in a loop. `free_fresh_owned_str_arg` is Call/MethodCall-
            // only and `cap > 0`-guarded, so a place expression (identifier /
            // field — owned by its binding) or a rodata literal is left
            // untouched (no double-free). The builder is already at the
            // post-write merge block, so every byte read dominates the free.
            // `rhs_stages_fstr_acc` excludes a struct/enum `.to_string()`: it
            // lowers via the synthetic f-string whose accumulator already owns a
            // scope-exit cleanup, so freeing here too would double-free (a
            // scalar/`String` `.to_string()` does NOT stage the acc and is still
            // freed). A direct f-string arg is an `InterpolatedStringLit`, not a
            // Call/MethodCall, so it is excluded upstream by
            // `expr_yields_fresh_owned_temp` regardless.
            if !self.rhs_stages_fstr_acc(&args[0].value) {
                self.free_fresh_owned_str_arg(&args[0].value, val);
            }
        } else if val.is_pointer_value() {
            // Raw pointer treated as a NUL-terminated C string (shared types,
            // etc.): measure with `strlen`, then route the bytes through the
            // console chokepoint (capture-aware) instead of `printf("%s")` —
            // `nl` is appended by the write.
            let strlen_fn = self
                .module
                .get_function("strlen")
                .expect("strlen declared in Codegen::new");
            let slen = self
                .builder
                .build_call(strlen_fn, &[val.into_pointer_value().into()], "p.slen")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_int_value();
            self.emit_nul_safe_write(val.into_pointer_value(), slen, nl, false);
        } else if val.is_float_value() {
            // Render with Rust's shortest-round-trip `{}` formatting (via the
            // runtime `karac_runtime_f64_to_str`) so AOT output matches
            // `karac run` exactly — not C `printf`'s `%g` (6 significant
            // figures, lowercase `nan`). `format_f64_to_stack_buf` widens
            // f32→f64 and returns `(buf_ptr, len)`; written NUL-safely with
            // the trailing newline (`nl` is "" for `print`). Float text never
            // carries a NUL, but routing it through the same `fwrite` path
            // keeps the print surface uniform (and buffer-shared with printf).
            let (buf_ptr, len) = self.format_f64_to_stack_buf(val.into_float_value());
            self.emit_nul_safe_write(buf_ptr, len, nl, false);
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
        let tail = self.tail_ret_inner.take();
        let cond_val = self.compile_expr(condition)?.into_int_value();
        let fn_val = self.current_fn.unwrap();
        let then_bb = self.context.append_basic_block(fn_val, "then");
        let else_bb = self.context.append_basic_block(fn_val, "else");
        let merge_bb = self.context.append_basic_block(fn_val, "ifmerge");

        self.builder
            .build_conditional_branch(cond_val, then_bb, else_bb)
            .unwrap();

        self.builder.position_at_end(then_bb);
        self.tail_ret_inner = tail;
        let then_val = self.compile_block_with_frame(then_block)?;
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
            self.tail_ret_inner = tail;
            match &else_expr.kind {
                ExprKind::Block(blk) => self.compile_block_with_frame(blk)?,
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
        self.tail_ret_inner = None;
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

        let placeholder = self.context.i64_type().const_int(0, false).into();
        match (then_terminated, else_terminated) {
            // Both arms diverge (`return` / `unreachable()` / `todo()` on each
            // side): the merge block has no predecessors. Terminate it with
            // `unreachable` so the enclosing terminator guards (the fn-tail
            // `ret` in `compile_function`, `compile_block` between statements)
            // skip emitting a follow-on instruction — otherwise a
            // value-returning fn whose `if` both-diverges would emit
            // `ret <i64 placeholder>` against its real return type and fail
            // module verification (the gap-d failure class for branchy tails).
            (true, true) => {
                self.builder.build_unreachable().unwrap();
                Ok(placeholder)
            }
            // Exactly one arm diverges: the merge has a single live
            // predecessor, so the `if`-expression's value IS the live arm's
            // value (it dominates the merge — no phi needed). This is what
            // makes `if c { v } else { unreachable() }` evaluate to `v`
            // rather than the const-0 placeholder. `unwrap_or` covers the
            // value-less arm (e.g. a terminated `then` with no `else`).
            (true, false) => Ok(else_val.unwrap_or(placeholder)),
            (false, true) => Ok(then_val.unwrap_or(placeholder)),
            // Neither arm diverges: phi over both when the value types agree;
            // otherwise the `if` is in statement position (unit value) — fall
            // back to the const-0 placeholder.
            (false, false) => {
                if let (Some(tv), Some(ev)) = (then_val, else_val) {
                    let (tv, ev) = self.unify_int_branch_widths(tv, then_end_bb, ev, else_end_bb);
                    if tv.get_type() == ev.get_type() {
                        let phi = self.builder.build_phi(tv.get_type(), "ifval").unwrap();
                        phi.add_incoming(&[(&tv, then_end_bb), (&ev, else_end_bb)]);
                        return Ok(phi.as_basic_value());
                    }
                }
                Ok(placeholder)
            }
        }
    }

    /// Reconcile the LLVM int widths of an `if`/`if let`'s two branch values
    /// before the phi. The typechecker has ALREADY unified both branches to one
    /// Kāra type, so any LLVM width mismatch reachable here is a codegen
    /// representation artifact of that single type — never two genuinely
    /// different types. Two such artifacts both surface as a wide branch beside
    /// a narrow one:
    ///
    /// - a suffixless integer literal (`0`) lowers at the default `i64` width
    ///   (`const_int_for_suffix` keys off the suffix only), while the sibling
    ///   carries its real narrower type (`u8`, …) — `{ 0 } else { byte }`;
    /// - a narrow-int arithmetic expr keeps the i64 it is computed at
    ///   (`compile_narrow_int_binop` range-checks to the declared width but
    ///   leaves the value wide for boundary coercion — see its doc), while the
    ///   sibling is the bare narrow value — `if upper { b + 32 } else { b }`,
    ///   the ASCII case-fold surface.
    ///
    /// Either way the wider side's meaningful bits fit the narrower width (same
    /// Kāra type), so truncating it down is value-preserving and makes the phi's
    /// operands agree. The truncate is emitted in the *wider branch's
    /// predecessor* (before its terminating branch to the merge block) so the
    /// phi operand dominates its incoming edge; a const folds with no
    /// instruction, so its position is immaterial. The builder is restored to
    /// the caller's insert block (the merge).
    ///
    /// Without this, the merge falls through to the const-`0` placeholder and
    /// the WHOLE construct evaluates to `0` — originally self-hosting #7 (the
    /// lexer's `fn peek(ref self) -> u8 { if … { 0 } else { … } }` always
    /// returned 0, so every scan loop exited immediately); then the
    /// arithmetic-branch case (`to_lower` / case-fold), which the earlier
    /// `is_const()`-gated version still mis-lowered because it assumed a
    /// non-constant wide branch was typechecker-impossible — narrow-int
    /// arithmetic makes it routine.
    fn unify_int_branch_widths(
        &self,
        a: BasicValueEnum<'ctx>,
        a_pred: BasicBlock<'ctx>,
        b: BasicValueEnum<'ctx>,
        b_pred: BasicBlock<'ctx>,
    ) -> (BasicValueEnum<'ctx>, BasicValueEnum<'ctx>) {
        let (BasicValueEnum::IntValue(av), BasicValueEnum::IntValue(bv)) = (a, b) else {
            return (a, b);
        };
        let (aw, bw) = (av.get_type().get_bit_width(), bv.get_type().get_bit_width());
        if aw > bw {
            (
                self.truncate_branch_value_in_pred(av, bv.get_type(), a_pred),
                b,
            )
        } else if bw > aw {
            (
                a,
                self.truncate_branch_value_in_pred(bv, av.get_type(), b_pred),
            )
        } else {
            (a, b)
        }
    }

    /// Truncate a phi-bound integer branch value down to `target`, emitting the
    /// `trunc` at the END of its predecessor block (before that block's
    /// terminating branch to the merge) so the result dominates the phi's
    /// incoming edge — a `trunc` in the merge block itself would not. A
    /// compile-time constant folds with no instruction emitted, so the
    /// repositioning is a harmless no-op for it. The builder's insert position
    /// is saved and restored, so the caller (positioned at the merge block)
    /// sees no change. Shared by the `if` / `if let` two-arm merge and the
    /// `match` N-arm merge (`unify_int_match_arm_widths`).
    fn truncate_branch_value_in_pred(
        &self,
        v: IntValue<'ctx>,
        target: IntType<'ctx>,
        pred: BasicBlock<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        let resume = self.builder.get_insert_block();
        match pred.get_terminator() {
            Some(term) => self.builder.position_before(&term),
            None => self.builder.position_at_end(pred),
        }
        let t = self
            .builder
            .build_int_truncate(v, target, "ifw.trunc")
            .unwrap();
        if let Some(bb) = resume {
            self.builder.position_at_end(bb);
        }
        t.into()
    }

    /// Harmonize the LLVM int widths of a `match`'s arm values before the phi —
    /// the N-arm analog of [`unify_int_branch_widths`]. Same invariant (the
    /// typechecker unified every arm to one Kāra type) and same artifact
    /// (suffixless literals / narrow-int arithmetic leave some arms i64 beside
    /// narrow siblings). Truncates every arm value wider than the narrowest to
    /// that narrowest width, each in its own predecessor block. Arms that are
    /// not integers, or already share the minimum width, pass through untouched.
    pub(super) fn unify_int_match_arm_widths(
        &self,
        arms: &mut [(BasicValueEnum<'ctx>, BasicBlock<'ctx>)],
    ) {
        let min_width = arms
            .iter()
            .filter_map(|(v, _)| match v {
                BasicValueEnum::IntValue(iv) => Some(iv.get_type().get_bit_width()),
                _ => None,
            })
            .min();
        let Some(min_width) = min_width else {
            return;
        };
        let target = match min_width {
            8 => self.context.i8_type(),
            16 => self.context.i16_type(),
            32 => self.context.i32_type(),
            128 => self.context.i128_type(),
            _ => self.context.i64_type(),
        };
        for (v, bb) in arms.iter_mut() {
            if let BasicValueEnum::IntValue(iv) = v {
                if iv.get_type().get_bit_width() > min_width {
                    *v = self.truncate_branch_value_in_pred(*iv, target, *bb);
                }
            }
        }
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

        // Monotone-variable BCE (control_flow_bce.rs § monotone scan):
        // load each qualifying variable's loop-entry value here in the
        // preheader; the matching `llvm.assume`s are emitted at body
        // entry below.
        let mono_vars = self.collect_monotone_index_vars(Some(condition), body);
        let mono_inits = self.load_monotone_inits(&mono_vars);

        self.builder.build_unconditional_branch(cond_bb).unwrap();

        self.loop_stack.push(LoopFrame {
            label: label.map(str::to_string),
            continue_bb: cond_bb,
            break_bb: exit_bb,
            result_slot: None,
            cleanup_depth: self.scope_cleanup_actions.len(),
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
        // Monotone facts: `x >= / <= its preheader value`, consumed by
        // LLVM's range passes to fold checks the source guard can't
        // express (conditionally-updated write heads / cursors).
        self.emit_monotone_assumes(&mono_inits);
        // Binary-search midpoint facts: a strict `lo < hi` guard lets a
        // `let mid = lo + (hi - lo) / 2` binding in the body assert
        // `lo <= mid < hi`, folding the `nums[mid]` bounds check that
        // interval-based CVP can't (control_flow_bce.rs § midpoint).
        let binsearch_guard = Self::binsearch_guard_pair(condition);
        if let Some(pair) = binsearch_guard.clone() {
            self.binsearch_guard_stack.push(pair);
        }
        // Per-iteration scope frame, same shape as compile_for_range — see
        // its comment for the leak rationale.
        self.scope_cleanup_actions.push(Vec::new());
        self.compile_block(body)?;
        // Pop the bounds we pushed for this loop; restore the surrounding
        // scope's stack untouched. Nested loops therefore see only their
        // own and outer-loop bounds, never inner-loop leftovers.
        for _ in 0..pushed_count {
            self.asserted_index_bounds.pop();
        }
        if binsearch_guard.is_some() {
            self.binsearch_guard_stack.pop();
        }
        // Small constant-trip counted loop → hint LLVM to fully unroll
        // (B-2026-06-17-7): the back-edge branch built below carries
        // `llvm.loop.unroll.full` so a loop like kata:37's `while d <= 9`
        // unrolls the way rustc unrolls its equivalent (worth ~1.34x on
        // that bench). Advisory-only — LLVM ignores it if it can't prove a
        // small constant trip count. Computed while `condition`/`body` are
        // in scope; applied to the back-edge instruction.
        let wants_full_unroll = self.while_loop_wants_full_unroll(condition, body);
        let body_has_terminator = self
            .builder
            .get_insert_block()
            .unwrap()
            .get_terminator()
            .is_some();
        if !body_has_terminator {
            self.drain_top_frame_with_emit();
            let back_edge = self.builder.build_unconditional_branch(cond_bb).unwrap();
            if wants_full_unroll {
                self.attach_unroll_full_metadata(back_edge);
            }
        } else {
            self.scope_cleanup_actions.pop();
        }

        self.loop_stack.pop();
        self.builder.position_at_end(exit_bb);
        Ok(self.context.i64_type().const_int(0, false).into())
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
            cleanup_depth: self.scope_cleanup_actions.len(),
        });

        self.builder.build_unconditional_branch(loop_bb).unwrap();
        self.builder.position_at_end(loop_bb);
        // Per-iteration scope frame, same shape as compile_for_range — see
        // its comment for the leak rationale (body-local shared-struct
        // lets re-bound on every iteration would otherwise climb refcount
        // N×K and pin the chain). Drained just before the back-edge to
        // `loop_bb`.
        self.scope_cleanup_actions.push(Vec::new());
        self.compile_block(body)?;
        let body_has_terminator = self
            .builder
            .get_insert_block()
            .unwrap()
            .get_terminator()
            .is_some();
        if !body_has_terminator {
            self.drain_top_frame_with_emit();
            self.builder.build_unconditional_branch(loop_bb).unwrap();
        } else {
            self.scope_cleanup_actions.pop();
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
            cleanup_depth: self.scope_cleanup_actions.len(),
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
            // Drain the frames INSIDE the loop being exited (per-iteration
            // frame + any nested block / `if let` / match-arm frames between
            // here and the loop boundary) — the back-edge / scope-end drains
            // are on paths this branch skips. Emit-only: the compile-time
            // stack is untouched, the fall-through path keeps its own drains.
            self.emit_scope_cleanup_from(frame.cleanup_depth);
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
            // Same early-exit drain as `compile_break`: `continue` jumps to
            // the loop header, skipping the body-end back-edge drain.
            self.emit_scope_cleanup_from(frame.cleanup_depth);
            self.builder
                .build_unconditional_branch(frame.continue_bb)
                .unwrap();
        }
        Ok(zero.into())
    }
}
