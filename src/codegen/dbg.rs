//! `dbg()` lowering for the compiled backends — design.md § `dbg()`.
//!
//! B-2026-08-23-18. Until this existed, `karac build` / `karac run` REFUSED any
//! program containing `dbg` (B-2026-08-23-16 put that refusal in place of a
//! silent constant-0 miscompile) and only `karac run --interp` executed it. The
//! interpreter's `eval_builtin_dbg` is the oracle this matches.
//!
//! The shape of the lowering, and why it is small:
//!
//!  - **Expression text, file, line** are compile-time constants. Codegen holds
//!    `source_text` / `source_filename` and slices the argument's span exactly
//!    as the interpreter does.
//!  - **The value** renders through the SAME Display-function family that
//!    `println` uses, switched into `Debug` mode (`DisplayState::debug_render`).
//!    `Debug` and `Display` differ at exactly two leaves — `String` and `char`
//!    are quoted — and agree on every compound shape, so one walker serves both
//!    and the quoting happens in the runtime via Rust's own `{:?}`.
//!  - **The envelope** — terminal vs JSON, the `[task:N …]` tag, the trailing
//!    newline, the single atomic `write(2)` — is `karac_dbg_emit` in the
//!    runtime, not IR built here.
//!
//! What is deliberately NOT here: a fallback. If the argument's type is not
//! available or its renderer cannot be synthesized, this REFUSES with a message
//! naming the shape. Every bug in this cluster (B-2026-08-23-14, -16,
//! B-2026-07-31-9) was a surface that looked present and silently was not, so a
//! partial lowering must fail loudly rather than print a placeholder.

use crate::ast::*;
use crate::token::Span;
use inkwell::values::BasicValueEnum;
use inkwell::AddressSpace;

impl<'ctx> super::Codegen<'ctx> {
    /// Lower `dbg(x)`: emit the diagnostic line, then hand back `x` unchanged.
    ///
    /// `dbg` is an identity function with a side effect (design.md § `dbg()`),
    /// so the argument is compiled exactly once and its value is the result —
    /// `dbg(compute())` calls `compute` once, and `dbg(41) + 1` is 42.
    pub(super) fn compile_dbg(
        &mut self,
        args: &[CallArg],
        call_span: &Span,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let i64_t = self.context.i64_type();
        // `dbg()` with no argument evaluates to Unit and prints nothing —
        // mirrors the interpreter, which returns `Value::Unit` rather than
        // erroring.
        let Some(arg) = args.first() else {
            return Ok(i64_t.const_zero().into());
        };
        if args.len() > 1 {
            return Err(format!(
                "codegen: dbg() takes 0 or 1 argument(s), found {} at {}:{}",
                args.len(),
                call_span.line,
                call_span.column
            ));
        }

        let key = (arg.value.span.offset, arg.value.span.length);
        // The argument's static type, recorded by the typechecker when it
        // applied dbg's identity rule. Absent means the call never reached that
        // rule — refuse rather than guess a renderer.
        let Some(arg_te) = self.span_tables.dbg_arg_type_exprs.get(&key).cloned() else {
            return Err(format!(
                "codegen: `dbg` at {}:{} has no recorded argument type, so its \
                 `Debug` renderer cannot be synthesized. This is a compiler gap, \
                 not a program error — `karac run --interp` executes it \
                 correctly. Please report it with the expression.",
                call_span.line, call_span.column
            ));
        };
        let type_name = self
            .span_tables
            .dbg_arg_type_names
            .get(&key)
            .cloned()
            .unwrap_or_else(|| "?".to_string());

        // FAIL CLOSED on a shape whose renderer does not exist, BEFORE emitting
        // anything. `dbg` accepts a strictly wider domain than `println` does —
        // the typechecker deliberately puts no `Display` bound on it, because
        // `dbg(some_struct)` is most of what dbg is for — so it reaches types
        // the Display family was never built for. A `shared struct` is the
        // measured example: it has no `Display` impl at all (`println(sh)` is a
        // typecheck error), and its name IS in `struct_field_names` while its
        // LLVM type is in `shared_types` rather than `struct_types`, so the
        // struct renderer panicked the compiler with "struct type registered".
        //
        // Refusing here is the rule this whole cluster exists to enforce: every
        // bug in it (B-2026-08-23-14, -16, B-2026-07-31-9) was a surface that
        // LOOKED present and silently was not, so an unlowered shape must say
        // so and name itself rather than print a placeholder or crash.
        if let Some(shape) = self.dbg_unsupported_shape(&arg_te) {
            return Err(format!(
                "codegen: `dbg` at {}:{} cannot render a value of type `{shape}` yet \
                 — the compiled backends have no `Debug` renderer for it. Every \
                 other shape compiles; `karac run --interp` renders this one. \
                 Printing a placeholder instead is deliberately not an option \
                 (B-2026-08-23-18).",
                call_span.line, call_span.column
            ));
        }

        // Compile the argument ONCE — it is both what gets printed and what
        // gets returned.
        let value = self.compile_expr(&arg.value)?;

        // Expression text: the same `span.offset .. offset + length` slice the
        // interpreter takes, with the same `<expr>` fallback when no source
        // text was handed to codegen.
        let expr_text = self
            .source_text
            .as_ref()
            .and_then(|src| {
                src.get(arg.value.span.offset..arg.value.span.offset + arg.value.span.length)
            })
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| "<expr>".to_string());
        let file = self
            .source_filename
            .clone()
            .filter(|f| !f.is_empty())
            .unwrap_or_else(|| "<unknown>".to_string());

        // Render the value through the Debug-mode renderer family. The flag is
        // saved/restored around the synthesis so a `println` elsewhere in the
        // same module still gets Display functions, and the recursion into
        // element/field types inherits Debug for the whole subtree.
        let fn_val = self
            .current_fn
            .ok_or_else(|| "codegen: `dbg` outside a function body".to_string())?;
        let slot = self.create_entry_alloca(fn_val, "dbg.val", value.get_type());
        self.builder.build_store(slot, value).unwrap();

        // OWNERSHIP. `dbg` does NOT consume its argument — it is classified
        // `Ref` alongside the print family (`ownership.rs`
        // § `collect_callee_param_modes`), because a construct that is stripped
        // from release builds must not change what a program means by being
        // present. So for a PLACE-expression argument the value handed back
        // cannot be the argument's own descriptor: the binding still owns the
        // buffer and frees it at scope exit, while the returned temporary gets a
        // cleanup of its own — two frees of one allocation.
        //
        // Measured, with the descriptor returned directly: `dbg(vs)` on a
        // `Vec[i64]` binding and `dbg(hs)` on a heap `String` both aborted with
        // "double free detected in tcache 2"; `dbg(mp)` on a `Map` SEGFAULTED
        // (the handle is freed twice); `Option[String]` aborted the same way.
        // A struct, a tuple, and a plain struct were all unaffected — codegen
        // queues no cleanup for a temporary of those shapes — which is why the
        // copy below is scoped to the shapes that actually own a freeable
        // resource rather than applied to everything.
        //
        // This runs in the STRIPPED build too, and must: stripping removes the
        // diagnostic, not the expression, and `let w = dbg(vs)` still has to
        // hand `w` something it may free exactly once. Skipping it in the
        // stripped path is exactly what made `karac build --release` abort on a
        // program whose debug build was clean.
        //
        // The alternative — suppressing the SOURCE's cleanup, so the returned
        // temporary becomes the owner — is what the ownership checker's old
        // "moved here" reading implied, and it is worse: `dbg(vs); vs.push(1)`
        // would then push into a freed buffer, a use-after-free where today
        // there is a double free. Copying keeps both values independently
        // valid, which is the only reading under which `dbg` is transparent.
        let returned =
            if Self::dbg_arg_is_place(&arg.value) && self.dbg_result_needs_owned_copy(&arg_te) {
                let dst = self.create_entry_alloca(fn_val, "dbg.own", value.get_type());
                // The OWNING clone, not the plain dispatcher. `dbg`'s copy is a
                // DUPLICATION -- the binding and the returned temporary must
                // both be independently valid -- which is exactly the
                // distinction `emit_owning_clone_fn_for_type_expr` exists to
                // draw: for a `shared struct` handle it retains (rc_inc), where
                // the plain dispatcher deliberately does not, because its
                // twenty-odd other callers are move/transfer sites that manage
                // RC themselves. For every other shape the two are the same
                // function. Without this a `dbg` of a shared handle aliased the
                // binding's box with no increment and the two scope-exit decs
                // freed it once too often -- MEASURED as `malloc(): unaligned
                // tcache chunk detected` on a two-link `Option[shared]` list,
                // the same symptom B-2026-07-28-10 records for this class.
                let clone_fn = self.emit_owning_clone_fn_for_type_expr(&arg_te);
                self.builder
                    .build_call(clone_fn, &[slot.into(), dst.into()], "dbg.clone")
                    .unwrap();
                self.builder
                    .build_load(value.get_type(), dst, "dbg.owned")
                    .unwrap()
            } else {
                value
            };

        // design.md § `dbg()`: "Stripped from release builds". Everything below
        // — the renderer synthesis, the constant strings, the runtime call — is
        // what a release build does not pay for. The argument is still compiled
        // (it is the result, and may have effects) and its ownership is already
        // settled above.
        if Self::dbg_stripped() {
            return Ok(returned);
        }

        let prev_mode = self.display.debug_render;
        self.display.debug_render = true;
        let render_fn = self.emit_display_fn_for_type_expr(&arg_te);
        self.display.debug_render = prev_mode;

        let (acc, _sval) = self.render_via_display_fn(render_fn, slot);

        let vec_ty = self.vec_struct_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let val_data = self
            .builder
            .build_load(
                ptr_ty,
                self.builder
                    .build_struct_gep(vec_ty, acc, 0, "dbg.v.dp")
                    .unwrap(),
                "dbg.v.data",
            )
            .unwrap()
            .into_pointer_value();
        let val_len = self
            .builder
            .build_load(
                i64_t,
                self.builder
                    .build_struct_gep(vec_ty, acc, 1, "dbg.v.lp")
                    .unwrap(),
                "dbg.v.len",
            )
            .unwrap()
            .into_int_value();

        let (file_p, file_l) = self.dbg_global_str(&file, "dbg.file");
        let (expr_p, expr_l) = self.dbg_global_str(&expr_text, "dbg.expr");
        let (type_p, type_l) = self.dbg_global_str(&type_name, "dbg.type");
        let line = i64_t.const_int(call_span.line as u64, false);

        self.builder
            .build_call(
                self.runtime_fns.karac_dbg_emit_fn,
                &[
                    file_p.into(),
                    file_l.into(),
                    line.into(),
                    expr_p.into(),
                    expr_l.into(),
                    type_p.into(),
                    type_l.into(),
                    val_data.into(),
                    val_len.into(),
                ],
                "",
            )
            .unwrap();

        // The rendered text is a one-shot temporary; the line has been written.
        // Freeing here keeps `dbg` off LeakSanitizer's report (the Linux
        // `memory-sanitizer` job is the authoritative leak gate).
        self.builder
            .build_call(self.runtime_fns.free_fn, &[val_data.into()], "")
            .unwrap();

        Ok(returned)
    }

    /// The first type in `te` (itself or nested) for which no `Debug` renderer
    /// can be synthesized, or `None` when the whole shape is renderable.
    ///
    /// Today that means a `shared enum` (its payload lives in the RC box behind
    /// a variant tag and has no renderer yet) and a HEADERLESS shared struct
    /// (whose field base is a per-function property, so it cannot be baked into
    /// one program-wide cached renderer — see
    /// `emit_shared_struct_debug_display_fn`). Ordinary `shared struct` /
    /// `par struct` handles DO render as of B-2026-08-24-2, including
    /// self-referential `Option[shared]` fields. Nested positions are walked
    /// too, so `Vec[Sh]` and `Option[Sh]` refuse at the point of the offending
    /// element rather than panicking one level down.
    fn dbg_unsupported_shape(&self, te: &TypeExpr) -> Option<String> {
        let TypeKind::Path(p) = &te.kind else {
            // Tuples: check every element.
            if let TypeKind::Tuple(elems) = &te.kind {
                return elems.iter().find_map(|e| self.dbg_unsupported_shape(e));
            }
            return None;
        };
        if let Some(seg) = p.segments.last() {
            let shared_info = self.type_decls.shared_types.get(seg);
            let is_shared_name =
                self.type_decls.shared_type_decl_names.contains(seg) || shared_info.is_some();
            // A shared STRUCT renders (B-2026-08-24-2) unless it is headerless
            // here — the headerless niche drops the refcount word, moving user
            // field 0 to heap index 0, and `headerless_here` is answered per
            // FUNCTION while the renderer is cached program-wide. Refusing is
            // the honest outcome: the alternative is a cached renderer that
            // reads every field at the wrong offset in some other function.
            let renderable_shared_struct = shared_info
                .is_some_and(|i| !i.is_enum && !i.has_weak_header)
                && !self.headerless_here(seg);
            if is_shared_name && !renderable_shared_struct {
                return Some(seg.clone());
            }
        }
        p.generic_args.as_ref().and_then(|args| {
            args.iter().find_map(|a| match a {
                GenericArg::Type(t) => self.dbg_unsupported_shape(t),
                _ => None,
            })
        })
    }

    /// Whether the `dbg` argument is a PLACE expression — something another
    /// binding owns — rather than a temporary this call produced. A temporary
    /// is already the sole owner of whatever it holds, so it is handed straight
    /// back; a place needs the owned copy above.
    fn dbg_arg_is_place(e: &Expr) -> bool {
        matches!(
            &e.kind,
            ExprKind::Identifier(_)
                | ExprKind::FieldAccess { .. }
                | ExprKind::TupleIndex { .. }
                | ExprKind::Index { .. }
        )
    }

    /// Whether a returned value of this type owns a resource codegen would free
    /// at scope exit — the shapes measured to double-free above. Deliberately a
    /// closed list of head names rather than a general "owns heap" predicate:
    /// `emit_clone_fn_for_type_expr` covers primitives, `String`, `Vec`, `Map`,
    /// `Set` and tuples but NOT user structs, and the struct shapes do not need
    /// it, so a broader test would reach a clone path that does not exist.
    fn dbg_result_needs_owned_copy(&self, te: &TypeExpr) -> bool {
        let TypeKind::Path(p) = &te.kind else {
            return false;
        };
        let Some(head) = p.segments.last().map(String::as_str) else {
            return false;
        };
        // B-2026-08-24-2 — a `shared struct` / `par struct` handle owns a
        // refcount, so it needs the copy exactly as a Vec or String does. It is
        // not in the builtin list below because that list is spelled by NAME
        // and a shared type's name is the user's; asking `shared_types` is what
        // makes it work for any of them. Without this the returned temporary
        // aliased the binding's handle with no increment and the two cleanups
        // decremented one box twice.
        if self.type_decls.shared_types.contains_key(head) {
            return true;
        }
        matches!(
            head,
            "Vec"
                | "VecDeque"
                | "String"
                | "str"
                | "Map"
                | "Set"
                | "SortedMap"
                | "SortedSet"
                | "Option"
                | "Result"
        )
    }

    /// A deduped global string plus its byte length, for the constant text
    /// pieces of a `dbg` line.
    fn dbg_global_str(
        &mut self,
        s: &str,
        name: &str,
    ) -> (
        inkwell::values::PointerValue<'ctx>,
        inkwell::values::IntValue<'ctx>,
    ) {
        let g = self.builder.build_global_string_ptr(s, name).unwrap();
        (
            g.as_pointer_value(),
            self.context.i64_type().const_int(s.len() as u64, false),
        )
    }

    /// Whether `dbg` lines are stripped from this build (design.md § `dbg()`:
    /// "Stripped from release builds (`karac build --release`)").
    ///
    /// Read from the environment rather than threaded as a parameter, matching
    /// how every other build-mode knob reaches codegen (`KARAC_AUTO_PAR`,
    /// `KARAC_BCE_*`, `KARAC_MAP_TAG`, …). `karac build --release` sets it.
    pub(super) fn dbg_stripped() -> bool {
        std::env::var("KARAC_STRIP_DBG").as_deref() == Ok("1")
    }
}
