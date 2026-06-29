//! Expression compilation.
//!
//! Houses `compile_expr` (the big per-expression-kind dispatch),
//! `compile_offset_of` (the `offset_of!` intrinsic), `compile_path_expr`
//! (multi-segment-path-as-value lowering), `compile_question` (`?`
//! propagation), `emit_error_trace_push` /
//! `ensure_source_filename_global` (error-return-trace runtime
//! integration), `compile_struct_init` (struct literal), and the SOA
//! collection constructors `compile_soa_new` / `compile_soa_method`.

use crate::ast::*;

use inkwell::types::BasicTypeEnum;
use inkwell::values::{BasicValueEnum, FunctionValue, PointerValue};
use inkwell::AddressSpace;

use super::state::{SoaGroup, SoaLayout, VarSlot};

impl<'ctx> super::Codegen<'ctx> {
    pub(super) fn compile_expr(&mut self, expr: &Expr) -> Result<BasicValueEnum<'ctx>, String> {
        // Level 2 crash diagnostics: record the span of the expression being
        // compiled so `emit_panic` can report `panic at <file>:<line>:<col>`.
        // The headline panic sites (index OOB, unwrap-None, divide-by-zero,
        // Map missing key, slice range) emit their guard inside *this*
        // `compile_expr` call, so the span is exact for them. Cheap: a Span is
        // four `usize`s; this just stores a clone of the current node's span.
        self.current_span = Some(expr.span.clone());
        // Level 2 crash diagnostics — Part 2: stamp the DWARF source location
        // for the instructions this expression is about to emit (no-op unless
        // debug info is enabled, and self-guarded so it only attaches inside
        // the active subprogram's own function). Span line/column are 1-indexed.
        self.di_set_location(expr.span.line as u32, expr.span.column as u32);
        match &expr.kind {
            ExprKind::Integer(n, sfx) => Ok(self.const_int_for_suffix(*n, *sfx).into()),
            ExprKind::Float(f, sfx) => Ok(self.const_float_for_suffix(*f, *sfx).into()),
            // char lowers to an i32 holding the Unicode scalar value. The
            // earlier fallthrough emitted `i64 0` for every char literal,
            // breaking `let c: char = 'A'; println(f"{c}")` (printed `0`)
            // and any downstream arithmetic / comparison against the
            // literal value. Width parity with `s.chars()`'s decoded
            // i32 (`compile_for_string_chars_inner`) so a CharLit and a
            // chars-iter binding share a codegen type for the print and
            // f-string char-arm pickup. `*c as u64` widens the surrogate-
            // free `char` to fit the const_int sign-agnostic constructor.
            ExprKind::CharLit(c) => Ok(self.context.i32_type().const_int(*c as u64, false).into()),
            // `b'X'` lowers to an i8-width LLVM constant — width parity with
            // the `u8` type the typechecker assigns. Sign-agnostic (LLVM
            // integer types are bit-width-only); the `is_signed` flag on
            // `const_int` doesn't affect storage.
            ExprKind::ByteLit(b) => Ok(self
                .context
                .i8_type()
                .const_int(u64::from(*b), false)
                .into()),
            ExprKind::Bool(b) => Ok(self
                .context
                .bool_type()
                .const_int(u64::from(*b), false)
                .into()),
            ExprKind::StringLit(s) => {
                // NUL-safe global (L5): `build_global_string_ptr` would
                // truncate `s` at an interior NUL (C-string semantics); the
                // byte-array global preserves all `s.len()` bytes.
                let data_ptr = self.build_str_bytes_global(s.as_bytes(), "str");
                let str_ty = self.vec_struct_type();
                let i64_t = self.context.i64_type();
                let len = i64_t.const_int(s.len() as u64, false);
                let cap_zero = i64_t.const_int(0, false); // cap=0 → static, don't free
                let mut agg = str_ty.get_undef();
                agg = self
                    .builder
                    .build_insert_value(agg, data_ptr, 0, "str.data")
                    .unwrap()
                    .into_struct_value();
                agg = self
                    .builder
                    .build_insert_value(agg, len, 1, "str.len")
                    .unwrap()
                    .into_struct_value();
                agg = self
                    .builder
                    .build_insert_value(agg, cap_zero, 2, "str.cap")
                    .unwrap()
                    .into_struct_value();
                Ok(agg.into())
            }
            // `c"..."` — NUL-terminated rodata bytes carried as a `{ptr, i64}`
            // slice-struct value: field 0 points at the N+1-byte constant
            // (the compiler-appended NUL is invisible at the surface), field
            // 1 is the source byte count N, statically known so `len()` is
            // O(1) per design.md § C-String Literals. `const_string` (not
            // `build_global_string_ptr`) because the payload is raw bytes —
            // `\xHH` escapes need not be valid UTF-8. No cap word and no
            // drop: the literal is `'static` rodata, never freed.
            ExprKind::CStringLit { bytes, .. } => {
                let i8_ty = self.context.i8_type();
                let arr_ty = i8_ty.array_type(bytes.len() as u32 + 1); // +1 NUL
                let data = self.context.const_string(bytes, true); // null-terminated
                let data_global = self.module.add_global(arr_ty, None, "cstr");
                data_global.set_initializer(&data);
                data_global.set_constant(true);
                data_global.set_linkage(inkwell::module::Linkage::Internal);

                let slice_ty = self.slice_struct_type();
                let i64_t = self.context.i64_type();
                let len = i64_t.const_int(bytes.len() as u64, false);
                let mut agg = slice_ty.get_undef();
                agg = self
                    .builder
                    .build_insert_value(agg, data_global.as_pointer_value(), 0, "cstr.ptr")
                    .unwrap()
                    .into_struct_value();
                agg = self
                    .builder
                    .build_insert_value(agg, len, 1, "cstr.len")
                    .unwrap()
                    .into_struct_value();
                Ok(agg.into())
            }
            ExprKind::InterpolatedStringLit(parts) => {
                // Build an empty String alloca, then append each part.
                let vec_ty = self.vec_struct_type();
                let i64_t = self.context.i64_type();
                let ptr_ty = self.context.ptr_type(AddressSpace::default());
                let fn_val = self.current_fn.unwrap();

                let acc = self.create_entry_alloca(fn_val, "fstr.acc", vec_ty.into());
                // Entry-block zero-init so a never-executed f-string (this
                // expr inside a `for`/`if` body that doesn't run) leaves the
                // accumulator `{null, 0, 0}`, not uninitialized stack — the
                // scope-exit cleanup's `cap > 0` guard then skips it instead
                // of freeing garbage. The eval-site init below stays for the
                // re-evaluated case (a loop body builds the f-string fresh
                // each iteration). See `zero_init_str_acc_at_entry`.
                self.zero_init_str_acc_at_entry(acc);
                // Initialize: {null, 0, 0} — empty heap string.
                let null = ptr_ty.const_null();
                let zero = i64_t.const_int(0, false);
                let data_pp = self
                    .builder
                    .build_struct_gep(vec_ty, acc, 0, "fstr.data.pp")
                    .unwrap();
                let len_p = self
                    .builder
                    .build_struct_gep(vec_ty, acc, 1, "fstr.len.p")
                    .unwrap();
                let cap_p = self
                    .builder
                    .build_struct_gep(vec_ty, acc, 2, "fstr.cap.p")
                    .unwrap();
                self.builder.build_store(data_pp, null).unwrap();
                self.builder.build_store(len_p, zero).unwrap();
                self.builder.build_store(cap_p, zero).unwrap();

                // Register acc for scope cleanup (non-zero cap will be freed).
                // vec_ty is the same struct type used for Vec/String. The
                // f-string accumulator is always `String` (Vec[u8]); element
                // type u8 has no heap content, so no recursive drop needed.
                let u8_ty: BasicTypeEnum<'ctx> = self.context.i8_type().into();
                self.track_vec_var(acc, Some(u8_ty));

                // Pre-sized fast path: when every expr part is side-effect-
                // free, render all parts to (ptr, len) pairs first, sum the
                // lengths, malloc ONCE at the exact size, then memcpy each
                // part at its running offset. The append-per-part fallback
                // below pays a grow (malloc + full re-copy) per appended
                // part for the canonical snapshot-concat `f"{cur}("` —
                // first append sizes the buffer to exactly `cur.len()`, the
                // one-byte second append re-grows and re-copies — i.e. two
                // mallocs + two copies where C-style assembly pays one of
                // each (kata-22 bench: ~2× of the clang mirror, attributed
                // to exactly this). Pre-sizing needs the purity gate
                // because a String part's (ptr, len) aliases its source
                // buffer until the deferred memcpy runs: a later part with
                // side effects (a call mutating/consuming that source)
                // could invalidate it, whereas the interpreter — and the
                // fallback path — snapshot each part's bytes before the
                // next part evaluates. Side-effect-free parts (identifier /
                // field / index / arithmetic / literal shapes) cannot
                // invalidate anything, and they are essentially every hot
                // f-string. Renders into per-site entry allocas (snprintf
                // bufs, char bufs) stay valid across the deferred copies —
                // each render site owns a distinct alloca.
                let presize_ok = parts.iter().all(|p| match p {
                    ParsedInterpolationPart::Text(_) => true,
                    ParsedInterpolationPart::Expr(e) => Self::fstr_part_is_side_effect_free(e),
                });
                if presize_ok {
                    // Pass 1: render every part (left-to-right, same
                    // evaluation order as the fallback).
                    let mut rendered: Vec<(
                        inkwell::values::PointerValue<'ctx>,
                        inkwell::values::IntValue<'ctx>,
                    )> = Vec::with_capacity(parts.len());
                    for part in parts {
                        match part {
                            ParsedInterpolationPart::Text(text) => {
                                if !text.is_empty() {
                                    // NUL-safe global (L5) — the memcpy below
                                    // copies `text.len()` bytes, so the global
                                    // must carry interior NULs verbatim.
                                    let gptr =
                                        self.build_str_bytes_global(text.as_bytes(), "fstr.text");
                                    let text_len = i64_t.const_int(text.len() as u64, false);
                                    rendered.push((gptr, text_len));
                                }
                            }
                            ParsedInterpolationPart::Expr(e) => {
                                let pair = self.fstr_render_part(e)?;
                                rendered.push(pair);
                            }
                        }
                    }
                    // total = Σ lens (literal lens are constants — LLVM
                    // folds the chain down to runtime-len adds only).
                    let mut total = i64_t.const_int(0, false);
                    for (_, len) in &rendered {
                        total = self.builder.build_int_add(total, *len, "fstr.tot").unwrap();
                    }
                    // alloc_bytes = max(total, 1): keeps `cap > 0` so the
                    // scope-exit free stays armed even for an all-empty
                    // result (cap == 0 means "non-owning" everywhere else).
                    let one = i64_t.const_int(1, false);
                    let is_zero = self
                        .builder
                        .build_int_compare(inkwell::IntPredicate::ULT, total, one, "fstr.tot.zero")
                        .unwrap();
                    let alloc_bytes = self
                        .builder
                        .build_select(is_zero, one, total, "fstr.alloc")
                        .unwrap()
                        .into_int_value();
                    let buf = self
                        .builder
                        .build_call(self.malloc_fn, &[alloc_bytes.into()], "fstr.buf")
                        .unwrap()
                        .try_as_basic_value()
                        .unwrap_basic()
                        .into_pointer_value();
                    // Pass 2: memcpy each part at its running offset.
                    let i8_ty = self.context.i8_type();
                    let mut offset = i64_t.const_int(0, false);
                    for (ptr, len) in &rendered {
                        let dest = unsafe {
                            self.builder
                                .build_gep(i8_ty, buf, &[offset], "fstr.dest")
                                .unwrap()
                        };
                        self.builder.build_memcpy(dest, 1, *ptr, 1, *len).unwrap();
                        offset = self
                            .builder
                            .build_int_add(offset, *len, "fstr.off")
                            .unwrap();
                    }
                    self.builder.build_store(data_pp, buf).unwrap();
                    self.builder.build_store(len_p, total).unwrap();
                    self.builder.build_store(cap_p, alloc_bytes).unwrap();
                } else {
                    for part in parts {
                        match part {
                            ParsedInterpolationPart::Text(text) => {
                                if !text.is_empty() {
                                    // NUL-safe global (L5) — `emit_string_append_raw`
                                    // copies `text.len()` bytes, so interior NULs
                                    // must survive in the global.
                                    let gptr =
                                        self.build_str_bytes_global(text.as_bytes(), "fstr.text");
                                    let text_len = i64_t.const_int(text.len() as u64, false);
                                    self.emit_string_append_raw(acc, gptr, text_len);
                                }
                            }
                            ParsedInterpolationPart::Expr(e) => {
                                let (src_ptr, src_len) = self.fstr_render_part(e)?;
                                self.emit_string_append_raw(acc, src_ptr, src_len);
                            }
                        }
                    }
                }

                // Load the final String struct from the accumulator alloca.
                let result = self.builder.build_load(vec_ty, acc, "fstr.result").unwrap();
                // Stage the acc pointer so a consuming Let / Assign whose
                // LHS is a tracked Vec/String slot can suppress the acc's
                // scope-exit cleanup (the LHS now owns the buffer; without
                // suppression both cleanups fire on the same pointer and
                // double-free hangs in macOS malloc_printf). Discard / pass-
                // through consumers (println(f"..."), function args, etc.)
                // leave the staged value in place — the next compile_expr
                // overwrites it, or the surrounding scope's cleanup walk
                // ignores it.
                self.last_fstr_acc = Some(acc);
                Ok(result)
            }
            ExprKind::Identifier(name) => {
                // Resolution order: const-generic param (slice 4 — when
                // an active monomorphization's `const_subst` binds the
                // name, lower to the LLVM constant of the matching
                // width); local variable (may shadow a const), then
                // unit enum variant, then top-level `const` (re-compile
                // the stored value expression at this use site so LLVM
                // folds it), then free-fn-name-as-value (Slice B
                // follow-up 2026-05-09 — `let f = my_free_fn;` lowers
                // to the fn's global pointer; consumers that take
                // fn-pointer slots use it as a typed indirect-call
                // target), and finally `load_variable` so the existing
                // "Undefined variable" diagnostic still fires for
                // genuinely unbound names.
                if let Some(cv) = self.const_subst.get(name) {
                    let cv = cv.clone();
                    return Ok(self.compile_primitive_const(&cv));
                }
                if self.variables.contains_key(name.as_str()) {
                    self.load_variable(name)
                } else if let Some(loaded) = self.try_load_module_binding(name) {
                    // Slice 9: module-level `let` / `let mut` bindings
                    // are real LLVM globals. The lookup precedes the
                    // `consts` arm because slice 3 of the module-let
                    // work registers these in the Const-class
                    // namespace alongside `const` items; the resolver
                    // disambiguates by item kind, and codegen mirrors
                    // by preferring the module-binding load when both
                    // tables resolve.
                    Ok(loaded)
                } else if let Some(ev) = self.try_unit_enum_variant(name) {
                    Ok(ev)
                } else if let Some(const_value) = self.consts.get(name).cloned() {
                    self.compile_expr(&const_value)
                } else if self.module.get_function(name).is_some() {
                    // Free fn name as a first-class value → a `{trampoline,
                    // null env}` closure fat pointer (B-2026-06-21-2), the same
                    // representation as a closure literal and as the
                    // argument-site / let-binding reifies (B-2026-06-20-1 /
                    // -06-21-1). Producing the fat pointer HERE — the one
                    // free-fn-as-value source — makes a bare fn name lower
                    // correctly in every value position at once: a `Fn(...)`
                    // return tail (`fn pick() -> Fn(..) { doubler }`), a struct
                    // field initializer (`H { f: doubler }`), a `Vec[Fn(..)]`
                    // element (`v.push(doubler)`), and a `let` RHS. Before this
                    // it emitted a raw `ptr`, which mismatched the 16-byte
                    // fat-pointer slot those positions expect (verifier error or
                    // — through a local — a silent wrong call). `Server.serve`
                    // resolves its handler name independently via
                    // `resolve_free_fn_for_handler_arg` (it never reaches this
                    // arm), so its raw-fn-ptr FFI ABI is unaffected.
                    let (fat, _) = self
                        .reify_named_fn_value(name)
                        .expect("a resolved module fn reifies to a fn value");
                    Ok(fat)
                } else {
                    self.load_variable(name)
                }
            }
            ExprKind::SelfValue => {
                // `self` is bound as an ordinary local by `compile_function`'s
                // parameter loop (impl methods prepend a `self: Type` param).
                self.load_variable("self")
            }
            ExprKind::Binary { op, left, right } => match op {
                // Short-circuit `and`/`or` per documented design
                // (roadmap.md:425, 429): RHS only evaluates when the
                // LHS doesn't already determine the result, so RHS
                // side-effects (panicking index, dropped fn call)
                // don't fire when short-circuited.
                BinOp::And | BinOp::Or => self.compile_short_circuit(op, left, right),
                _ => {
                    // Element-wise arithmetic on `Tensor[T, Shape]` — the
                    // Binary's *result* span is tensor-typed (a fresh tensor).
                    // Route to the tensor lowering, which compiles the operands
                    // as borrows and mallocs a fresh value-semantics result.
                    // Must precede the generic scalar operand compilation below
                    // (two tensor pointers would choke `compile_binop`).
                    if self
                        .tensor_typed_exprs
                        .contains_key(&(expr.span.offset, expr.span.length))
                    {
                        return self.compile_tensor_binop(op, left, right, &expr.span);
                    }
                    // Element-wise SQL three-valued-logic op on `Column[T]`
                    // — the Binary's *result* span is column-typed
                    // (`Column[T]` for arithmetic, `Column[bool]` for
                    // comparison). Route to the column lowering (operands
                    // borrowed; fresh value-semantics result with null
                    // propagation). Mirrors the tensor intercept above.
                    if self
                        .column_typed_exprs
                        .contains_key(&(expr.span.offset, expr.span.length))
                    {
                        return self.compile_column_binop(op, left, right, &expr.span);
                    }
                    let lhs = self.compile_expr(left)?;
                    let rhs = self.compile_expr(right)?;
                    // Vector binops aren't lowered to primitive method calls
                    // (only primitives are), so they reach here as raw
                    // `ExprKind::Binary` with no signedness context. Recover the
                    // element signedness from the `unsigned_vector_exprs`
                    // side-table (keyed by the left operand span) so unsigned
                    // comparisons / div / mod pick `ult`/`ugt` / `udiv` / `urem`.
                    if lhs.is_vector_value() && rhs.is_vector_value() {
                        let is_unsigned = self
                            .unsigned_vector_exprs
                            .contains(&(left.span.offset, left.span.length));
                        return self.compile_binop_typed(op, lhs, rhs, is_unsigned);
                    }
                    // Heap-payload enum `==`/`!=`: route to the variant-aware
                    // comparator so a `String`/`Vec` payload compares by content,
                    // not by pointer word (the word-wise `compile_struct_eq` path
                    // is only sound for unit/scalar-payload enums). Two routes to
                    // "has a heap payload": the bare-name `enum_has_heap_payload`
                    // (concrete enums — `Msg { Text(String) }`), and, for a
                    // *generic* operand whose instantiation the lowering pass
                    // recorded (`Option[String]`, `Result[_, String]`),
                    // `instantiated_enum_has_heap_payload` resolving the type
                    // arg. The instantiation also feeds `compile_enum_eq` so the
                    // `Some` payload rebuilds as a `String`, not an opaque word.
                    // Unresolvable operands and scalar enums keep the cheaper path.
                    if matches!(op, BinOp::Eq | BinOp::NotEq)
                        && lhs.is_struct_value()
                        && rhs.is_struct_value()
                    {
                        if let Some(en) = self
                            .enum_name_of_expr(left)
                            .or_else(|| self.enum_name_of_expr(right))
                        {
                            // Resolve the operands' instantiation, but only
                            // trust one whose outer name matches `en` — cheap
                            // defense-in-depth so a stale/foreign span-table
                            // entry can never route a different enum's type to
                            // `compile_enum_eq` (which would rebuild the payload
                            // at the wrong type). f-string interpolation spans
                            // are now absolute (B-2026-06-09-1), so the former
                            // cross-f-string collision no longer occurs; the
                            // name-keyed `enum_inst_var_types` remains the
                            // primary resolver for identifier operands. An
                            // unresolved/foreign inst degrades to the word-wise
                            // path.
                            let inst = self
                                .enum_inst_type_of_expr(left)
                                .or_else(|| self.enum_inst_type_of_expr(right))
                                .filter(|t| match &t.kind {
                                    crate::ast::TypeKind::Path(p) => {
                                        p.segments.last().map(String::as_str) == Some(en.as_str())
                                    }
                                    _ => false,
                                });
                            let heap = self.enum_has_heap_payload(&en)
                                || self.instantiated_enum_has_heap_payload(&en, inst.as_ref());
                            if heap {
                                return self.compile_enum_eq(
                                    op,
                                    &en,
                                    inst.as_ref(),
                                    lhs.into_struct_value(),
                                    rhs.into_struct_value(),
                                );
                            }
                        }
                    }
                    // Shared-struct structural `==` / `!=` (C1, B-2026-06-19-9).
                    // A `shared struct` is an RC heap pointer, so it misses the
                    // value-wise struct path above; recover the struct name from
                    // an operand and call a field-walk comparator through the
                    // pointer (matching the interpreter's structural
                    // `Value::SharedStruct` equality). Shared *enums* stay on the
                    // honest-Err path in `compile_binop_typed` (out of scope).
                    if matches!(op, BinOp::Eq | BinOp::NotEq)
                        && (lhs.is_pointer_value() || rhs.is_pointer_value())
                    {
                        if let Some((name, info)) = self
                            .shared_type_for_expr(left)
                            .or_else(|| self.shared_type_for_expr(right))
                        {
                            if !info.is_enum && lhs.is_pointer_value() && rhs.is_pointer_value() {
                                let eq_fn = self.emit_shared_struct_eq_fn(&name);
                                let a = lhs.into_pointer_value();
                                let b = rhs.into_pointer_value();
                                let r = self
                                    .builder
                                    .build_call(eq_fn, &[a.into(), b.into()], "sheq.call")
                                    .unwrap()
                                    .try_as_basic_value()
                                    .unwrap_basic()
                                    .into_int_value();
                                let out = if matches!(op, BinOp::NotEq) {
                                    self.builder.build_not(r, "sheq.ne").unwrap()
                                } else {
                                    r
                                };
                                return Ok(out.into());
                            }
                        }
                    }
                    self.compile_binop(op, lhs, rhs)
                }
            },
            ExprKind::Unary { op, operand } => {
                if matches!(op, UnaryOp::Deref) {
                    // Raw-pointer deref (`*const T` / `*mut T`): the operand's
                    // value IS the address, so we must emit a real `load` of the
                    // pointee. The lowering pass records the pointee `TypeExpr`
                    // keyed by the operand span for exactly the raw-pointer case
                    // (`Type::Pointer`); references never land in this table.
                    // Without this, `unsafe { *p }` returned the pointer value
                    // itself (B-2026-06-11-3).
                    let key = (operand.span.offset, operand.span.length);
                    if let Some(pointee_te) = self.raw_pointer_pointee_types.get(&key).cloned() {
                        let ptr_val = self.compile_expr(operand)?.into_pointer_value();
                        let pointee_ty = self.llvm_type_for_type_expr(&pointee_te);
                        let loaded = self
                            .builder
                            .build_load(pointee_ty, ptr_val, "rawptr.deref")
                            .map_err(|e| e.to_string())?;
                        return Ok(loaded);
                    }
                    // `*r` where `r` is a let-bound entry slot ref
                    // (`let r = m.entry(k).or_insert(d)`): r's alloca holds the
                    // slot pointer (`*mut V`), so load the pointer then load V
                    // through it (the two-step entry counter's read side).
                    if let ExprKind::Identifier(name) = &operand.kind {
                        if self.entry_slot_ref_vars.contains_key(name) {
                            let (slot_ptr, val_ty) = self.entry_slot_ref_ptr(name)?;
                            let v = self
                                .builder
                                .build_load(val_ty, slot_ptr, "entry.ref.deref")
                                .map_err(|e| e.to_string())?;
                            return Ok(v);
                        }
                    }
                    // `*r` for a `ref T` / `mut ref T` — `load_variable` already
                    // performs the two-step dereference (load alloca → load
                    // through ptr), so `compile_expr(operand)` already yields the
                    // inner value. Return it directly.
                    return self.compile_expr(operand);
                }
                // Negative integer literals parse as `Neg(Integer(n))` and
                // the typechecker range-validates them as a UNIT — fold to a
                // single constant here. Compiling the positive half first
                // would wrap at the target width (`-2147483648i32`'s positive
                // half doesn't fit i32), and the checked-neg runtime trap
                // (design.md § Arithmetic Overflow) would then fire on a
                // literal that is in range as written. `n` is a positive
                // i64 literal, so `-n` cannot itself overflow.
                if matches!(op, UnaryOp::Neg) {
                    if let ExprKind::Integer(n, sfx) = &operand.kind {
                        return Ok(self.const_int_for_suffix(-*n, *sfx).into());
                    }
                    // Element-wise tensor negation — the result span is
                    // tensor-typed; lower to a fresh negated tensor.
                    if self
                        .tensor_typed_exprs
                        .contains_key(&(expr.span.offset, expr.span.length))
                    {
                        return self.compile_tensor_neg(operand, &expr.span);
                    }
                    // Element-wise column negation — the result span is
                    // column-typed; lower to a fresh negated column (nulls
                    // stay null).
                    if self
                        .column_typed_exprs
                        .contains_key(&(expr.span.offset, expr.span.length))
                    {
                        return self.compile_column_neg(operand, &expr.span);
                    }
                }
                let val = self.compile_expr(operand)?;
                self.compile_unaryop(op, val)
            }
            ExprKind::Call { callee, args } => self.compile_call(callee, args, &expr.span),
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => self.compile_if(condition, then_block, else_branch.as_deref()),
            ExprKind::While {
                label,
                condition,
                body,
                ..
            } => self.compile_while(label.as_deref(), condition, body),
            ExprKind::Loop { label, body, .. } => self.compile_loop(label.as_deref(), body),
            ExprKind::Break { label, value } => {
                self.compile_break(label.as_deref(), value.as_deref())
            }
            ExprKind::Continue { label } => self.compile_continue(label.as_deref()),
            ExprKind::Closure { params, body, .. } => {
                self.compile_closure(params, body, &expr.span)
            }
            ExprKind::Return(val) => {
                // Early-return cleanup parity with the function-end path
                // at `compile_function`: walk the full `scope_cleanup_actions`
                // stack and emit cleanup IR for every tracked binding before
                // building the return. Without this, heap-owning locals
                // (Vec data buffers, Map handles, RC-fallback heap boxes)
                // leak per-call whenever control exits via `return expr`
                // instead of falling through to the function body's tail.
                // Move-aware suppression mirrors the tail-return path: when
                // the return value is an Identifier naming a tracked
                // Vec / String, the caller now owns that buffer, so zeroing
                // the source's `cap` before cleanup skips the free for the
                // moved-out binding while still freeing every other tracked
                // local. Closes the 2026-05-13 bfs_sieve residual leak —
                // `return d` from inside a loop's match-arm body was
                // bypassing cleanup for `factors` / `bucket` / `visited` /
                // `queue`, leaking the entire per-call working set.
                //
                // Slice 2 (Phase 7 § *defer / errdefer codegen*): when the
                // return value is syntactically `Err(...)` or `None`, this
                // is an error-exit path — route through
                // `emit_scope_cleanup_for_error_path` so any in-scope
                // `errdefer { ... }` fires (phase 1) before the regular
                // drop+defer drain (phase 2). Other return shapes
                // (`Ok(v)`, `Some(v)`, plain values, or void return) stay
                // on the normal-exit drain. Detection is purely syntactic
                // — the typechecker has already rejected an Err/None
                // return in a non-Result/Option-returning function.
                let is_error_exit = val.as_deref().is_some_and(Self::is_error_exit_value);
                if let Some(e) = val {
                    // Move-out cleanup suppression (Vec/String `cap = 0`,
                    // Map/Set FreeMapHandle queue retract, user-`impl Drop`
                    // skip) is applied AFTER the return value is compiled —
                    // see the `suppress_*` calls following
                    // `maybe_defensive_copy_param_arg` below. The ordering is
                    // load-deferred on purpose: zeroing the source binding's
                    // `cap` slot BEFORE the value load corrupted the RETURNED
                    // header to `cap = 0`, so the caller's `cap > 0`-guarded
                    // free skipped and the moved-out buffer leaked once per
                    // call — every `return <vec/string>;` helper (B-2026-06-12-6,
                    // surfaced by the Linux LSan gate; macOS LSan-blind, and
                    // doing nothing produces no double-free, so it passed
                    // vacuously). The tail-expression sibling
                    // (`suppress_cleanup_for_tail_return`) already orders it
                    // load-then-suppress (compile body → load result →
                    // suppress); this matches it.
                    // `Option[shared T]` return compensation at an explicit
                    // `return expr;` — mirrors the per-branch TAIL machinery
                    // (`compile_tail_final_expr`): a bare tracked binding
                    // (`return head;`) is inc'd so the chain survives the
                    // binding's own scope-exit `RcDecOption` below with a
                    // net +1 for the caller; control-flow shapes re-arm the
                    // context for their branch finals; fresh sources
                    // (calls, `Some(...)` ctors — which inc shared
                    // payloads, `None`) compile plain. `compile_tail_final_expr`'s
                    // FieldAccess arm covers `return node.next;`
                    // (the niche field load is a bare ptr read — no inc —
                    // and the source field stays owned by the object, so
                    // the returned alias needs its own +1). Before this,
                    // every explicit-return alias shape under-counted and
                    // the caller read freed memory (`ret_ident`/`ret_field`
                    // repros, 2026-06-05 — pre-existing, surfaced by the
                    // niche-ABI slice's convergence tests; tail-position
                    // siblings were fixed by 426b8dc3/fca1e3ea).
                    // Chained borrow return at an explicit `return echo(t);`:
                    // admit the borrow-returning call (bypass the direct-use
                    // gate) — `v` is then the borrow `ptr`, returned directly
                    // below via the `returns_borrow_call` branch. Mirrors the
                    // tail-position handling in `compile_tail_final_expr`.
                    let returns_borrow_call =
                        self.current_fn_returns_ref && self.is_borrow_returning_call_expr(e);
                    let ret_opt_inner = self.current_fn_ret_option_inner_heap();
                    let v = if returns_borrow_call {
                        let prev = self.compiling_ref_return_let_rhs;
                        self.compiling_ref_return_let_rhs = true;
                        let v = self.compile_expr(e);
                        self.compiling_ref_return_let_rhs = prev;
                        v?
                    } else if ret_opt_inner.is_some() {
                        // `compile_tail_final_expr` performs the FULL
                        // `Option[shared]` return compensation for every shape:
                        // its Identifier arm incs a bare binding
                        // (`share_option_shared_ref_for_arg`), its FieldAccess
                        // arm incs a `node.next` alias
                        // (`share_option_shared_field_ref_for_arg`, gated on
                        // !structural_transfer), and its control-flow arms re-arm
                        // `tail_ret_inner` so branch leaves compensate themselves.
                        // It is the SAME entry the tail-position path
                        // (`fn f() -> Option[T] { node.next }`) uses, so an
                        // explicit `return node.next;` nets exactly +1. A second
                        // `share_option_shared_field_ref_for_arg(e, v)` here
                        // double-inc'd the field alias to +2 — the returned
                        // tail's head never reached rc 0 and the whole chain
                        // leaked 9 nodes/call (B-2026-06-12-6 cluster 5,
                        // `ret_field`); the tail-position sibling was single-inc
                        // and clean, which is why ONLY the explicit-return shape
                        // leaked. Removed 2026-06-12.
                        self.compile_tail_final_expr(e, ret_opt_inner)?
                    } else {
                        self.compile_expr(e)?
                    };
                    // Owned String/Vec PARAM returned by value (`return s;`
                    // where `s: String` is a parameter): the caller that
                    // passed `s` still owns its buffer (by-value header
                    // ABI), while the caller RECEIVING this return value
                    // binds-and-frees what we hand back — so hand back a
                    // deep copy, not the alias. No-op for every other
                    // return shape. See `emit_vecstr_defensive_copy`.
                    let v = self.maybe_defensive_copy_param_arg(e, v);
                    // Move-aware move-out suppression, applied post-compile (and
                    // post-defensive-copy) so the already-loaded `v` retains the
                    // source binding's real `cap` — the caller now owns and frees
                    // that buffer — while the source's own scope-exit
                    // `FreeVecBuffer` cap-guard reads the zeroed slot and skips.
                    // Mirrors `suppress_cleanup_for_tail_return`'s load-then-
                    // suppress order for the tail-expression case (which loads
                    // `result` first, then zeroes the source cap). The
                    // Vec/String arm flips an in-slot `cap = 0` sentinel; the
                    // Map/Set + user-`impl Drop` arms (Identifier source only)
                    // retract a queued `FreeMapHandle` / skip the UserDrop so the
                    // moved-out handle / drop-side-effect doesn't fire here — the
                    // caller fires it when its own binding goes out of scope.
                    self.suppress_source_vec_cleanup_for_arg(e);
                    if let ExprKind::Identifier(name) = &e.kind {
                        self.suppress_user_drop_for_var(name);
                        self.suppress_map_cleanup_for_tail_identifier(name);
                        // Return-again move-out (B-2026-06-22-2): an explicit
                        // `return f;` of a heap-env closure binding hands its RC
                        // env box to the caller — neutralize the source so the
                        // `emit_scope_cleanup()` below doesn't dec the box the
                        // caller now owns (runtime-null, branch-safe; sibling of
                        // the channel/SoA early-return suppressions here).
                        self.neutralize_moved_closure_env_slot(name);
                        // Aggregate-escape move-out (B-2026-06-22-2): an explicit
                        // `return h;` of an aggregate owner hands its struct (with
                        // the env boxes) to the caller — null the owned fields' env
                        // slots so their `FreeClosureEnv` no-ops below.
                        self.neutralize_moved_aggregate_env_slots(name);
                        // Channel-end `return rx;`: the moved-out `Sender`/
                        // `Receiver` is now the caller's; suppress the
                        // binding's scope-exit `DropChannelEnd` so its
                        // refcount decrement doesn't double-drop (sibling of
                        // the tail-expression case in
                        // `suppress_cleanup_for_tail_return`).
                        self.suppress_channel_drop_for_var(name);
                        // SoA move-out at an EARLY `return a;` in a return-SoA
                        // monomorph (the branch-leaf / multi-`return` follow-on):
                        // the moved-out 4-field SoA struct — sharing the group
                        // buffers — is now the caller's, so the source's
                        // `FreeSoaGroups` must not free them on THIS path. Use a
                        // runtime `cap = 0` sentinel (not the tail path's
                        // compile-time frame removal): the early-return cleanup
                        // frame is shared with the fall-through path, where `a` is
                        // NOT returned and must still be freed — frame removal
                        // would leak it there. Runs post-load (above), so the
                        // returned struct keeps the real cap and the caller frees
                        // once. Gated on the active return layout.
                        if matches!(self.return_layout, super::state::LayoutId::Soa(_)) {
                            self.neutralize_moved_soa_groups_slot(name);
                        }
                    }
                    // Move-aware suppression for a DIRECT `return f"..."`: the
                    // returned String buffer IS the f-string accumulator, now
                    // owned by the caller. The `suppress_source_vec_cleanup_for_arg`
                    // call above ran pre-compile (Identifier-only); the accumulator
                    // is staged only during the `compile_expr` just above, so
                    // suppress it here — before the scope-cleanup walk below frees
                    // it (the same double-free the struct-field site hit).
                    self.suppress_fstr_acc_if_moved_out(e);
                    // Contract `ensures` at an explicit `return expr` (design.md
                    // § Contracts), with `result` bound to the returned value —
                    // before the scope-exit cleanup below.
                    self.emit_ensures_checks(Some(v))?;
                    // Struct/impl `invariant` checks at the explicit return
                    // (rule 3), with `self` bound — same exit point as `ensures`.
                    // For a constructor, the returned value is bound as `self`.
                    self.emit_invariant_checks(Some(v))?;
                    if is_error_exit {
                        // Slice 4 (Phase 7 § *defer / errdefer codegen*):
                        // stage the Err payload for any in-scope
                        // binding-form errdefer. `Self::err_payload_from_value`
                        // extracts the first argument of a syntactic
                        // `Err(arg)` call (already compiled into `v`'s
                        // source). For `return None` / non-error returns
                        // this is `None`, and the error-path drain runs
                        // without any binding-form errdefer payload
                        // available — the no-binding form still fires.
                        //
                        // Slice 4 follow-up (b) — double-eval fix
                        // (2026-05-26). Slice 4 staged the payload by
                        // unconditionally re-compiling `payload_expr`,
                        // which double-evaluates side-effecting Err
                        // args like `return Err(some_fn_with_io());`
                        // The expr is now staged via two paths gated
                        // on `Self::is_pure_recompilable`:
                        //   - Pure (Identifier / Path / literal):
                        //     re-compile. Side-effect-free in source
                        //     semantics, so two evaluations produce the
                        //     same value and observable behaviour. The
                        //     IR is slightly bigger (one extra load /
                        //     constant emit), but value is source-typed
                        //     — preserves wider-E (`Result[T, String]`)
                        //     binding correctness.
                        //   - Impure (call / method / field access /
                        //     etc.): extract the i64-coerced payload
                        //     word from the constructed Err struct's
                        //     field 1. Single eval. Trade: wider-E
                        //     impure args see the i64-coerced w0
                        //     instead of the reconstructed source-
                        //     level value — same shape as the `?`
                        //     site's known limitation (a). Cross-
                        //     referenced in this entry's tracker
                        //     notes (`docs/implementation_checklist/
                        //     phase-7-codegen.md` line 96 closure).
                        let staged = Self::err_payload_from_value(e).and_then(|payload_expr| {
                            if Self::is_pure_recompilable(payload_expr) {
                                self.compile_expr(payload_expr).ok()
                            } else {
                                self.builder
                                    .build_extract_value(
                                        v.into_struct_value(),
                                        1,
                                        "errdefer_payload_w0",
                                    )
                                    .ok()
                            }
                        });
                        self.pending_errdefer_payload = staged;
                        self.emit_scope_cleanup_for_error_path();
                        self.pending_errdefer_payload = None;
                    } else {
                        self.emit_scope_cleanup();
                    }
                    // A2 slice 2b.3: inside a coroutine, an explicit `return v`
                    // routes to the signal + final-suspend block (the `ptr`
                    // ramp return is emitted in the shared suspend-return
                    // block); the Kāra value `v` is discarded (unit-only this
                    // slice). A coroutine fn is never `main`.
                    // Borrow return (`return s;` / `return u.field;` in a
                    // `-> ref T` fn): emit the ADDRESS of the borrow source,
                    // not the materialized `v` (B-2026-06-07-5). Computed
                    // after the scope-cleanup walk above — the source is a
                    // `ref` param (or a field through one), never a freed
                    // local, so its address is valid here.
                    let ref_ret_ptr = if !self.current_fn_returns_ref {
                        None
                    } else if returns_borrow_call {
                        // `v` is already the borrow ptr (compiled with the gate
                        // bypassed above); return it directly. Re-deriving via
                        // `compile_ref_return_ptr` would emit the call twice.
                        Some(v.into_pointer_value())
                    } else {
                        self.compile_ref_return_ptr(e)
                    };
                    if let Some(ctx) = self.coro_ctx {
                        // B-2026-06-19: carry the non-unit return value to the
                        // inline-drive caller via the completion slot before
                        // branching to the shared signal+suspend block (where
                        // `v` is no longer in scope). No-op for unit returns.
                        self.emit_coro_return_value_store(v);
                        self.builder
                            .build_unconditional_branch(ctx.coro_return_bb)
                            .unwrap();
                    } else if let Some(ptr) = ref_ret_ptr {
                        self.builder.build_return(Some(&ptr)).unwrap();
                    } else if self.current_fn_name == "main" && self.main_result_err_te.is_some() {
                        // `return Ok(())` / `return Err(e)` inside
                        // `main() -> Result[(), E]`: adapt to a process exit
                        // code rather than `ret`-ing the `{tag, …}` aggregate
                        // against `main`'s `i32` signature (B-2026-06-12-9).
                        self.emit_main_result_return(v);
                    } else if self.current_fn_ret_is_niche() {
                        // Niche-ABI return (`Option[shared T]` →
                        // nullable ptr): pack the conventional 4-i64
                        // Option value at the ret boundary. The ensures/
                        // invariant checks above already ran on the
                        // conventional shape.
                        let packed = self.option_value_to_niche_ptr(v);
                        self.builder.build_return(Some(&packed)).unwrap();
                    } else {
                        // Scalar width coercion at the ret boundary —
                        // internal values default to i64/f64 widths
                        // (literals, annotated `let` slots) while the
                        // signature declares the real width; without
                        // the trunc, `fn f() -> i32 { return 0; }`
                        // emits `ret i64 0` and fails verification.
                        // See `coerce_scalar_to_type`.
                        let v = self.coerce_to_current_ret_type(v);
                        self.builder.build_return(Some(&v)).unwrap();
                    }
                } else {
                    self.emit_scope_cleanup();
                    // `main` lowers to a C-ABI `i32 main()` (the process exit
                    // code), so a valueless `return;` reachable in `main` must
                    // emit `ret i32 0`, not `ret void` — otherwise the return
                    // instr's type mismatches the function signature and
                    // module verification fails ("ret void / i32"). Mirrors
                    // the implicit end-of-`main` return-zero in
                    // `compile_function`. Non-`main` void fns keep `ret void`.
                    // (phase-7-codegen.md — return-in-main fix.)
                    if let Some(ctx) = self.coro_ctx {
                        // A2 2b.3: valueless `return;` inside a coroutine →
                        // completion block (same as the value case).
                        self.builder
                            .build_unconditional_branch(ctx.coro_return_bb)
                            .unwrap();
                    } else if self.current_fn_name == "main" {
                        let zero = self.context.i32_type().const_int(0, false);
                        self.builder.build_return(Some(&zero)).unwrap();
                    } else {
                        self.builder.build_return(None).unwrap();
                    }
                }
                Ok(self.context.i64_type().const_int(0, false).into())
            }
            ExprKind::Block(block) | ExprKind::Seq(block) => {
                match self.compile_block_with_frame(block)? {
                    Some(v) => Ok(v),
                    None => Ok(self.context.i64_type().const_int(0, false).into()),
                }
            }
            ExprKind::FieldAccess { object, field } => self.compile_field_access(object, field),
            ExprKind::StructLiteral { path, fields, .. } => {
                let name = path.last().map(|s| s.as_str()).unwrap_or("");
                // Enum struct-variant construction `Enum.Variant { ... }`: the
                // qualifier (`path[len-2]`) names a known enum whose `Variant`
                // is one of its variants. Route to the enum-aggregate builder
                // (the typechecker/interpreter route the same shape); otherwise
                // it's a struct literal.
                let enum_variant = if path.len() >= 2 {
                    let enum_name = &path[path.len() - 2];
                    self.enum_layouts
                        .get(enum_name)
                        .filter(|l| l.tags.contains_key(name))
                        .map(|_| enum_name.clone())
                } else if !self.struct_types.contains_key(name) {
                    // Unqualified struct-variant construction `Variant { ... }`:
                    // the single-segment path carries only the bare variant
                    // name. When it doesn't name a struct, find the enum whose
                    // layout declares the variant (the typechecker already
                    // validated the reference, so a well-typed program yields a
                    // unique match — variant names are globally unique once the
                    // resolver has bound them). Mirrors the typechecker's
                    // `unqualified_enum_struct_variant` routing.
                    self.enum_layouts
                        .iter()
                        .find(|(_, l)| l.tags.contains_key(name))
                        .map(|(enum_name, _)| enum_name.clone())
                } else {
                    None
                };
                if let Some(enum_name) = enum_variant {
                    let variant = name.to_string();
                    self.compile_enum_struct_variant_init(&enum_name, &variant, fields)
                } else {
                    self.compile_struct_init(name, fields)
                }
            }
            ExprKind::ArrayLiteral(elems) => self.compile_array_literal(elems),
            ExprKind::PrefixCollectionLiteral { type_name, items } if type_name == "Vec" => {
                self.compile_vec_prefix_literal(items)
            }
            ExprKind::RepeatLiteral {
                type_name,
                value,
                count,
            } => self.compile_repeat_literal(type_name.as_deref(), value, count),
            ExprKind::Tuple(elems) => self.compile_tuple(elems),
            ExprKind::TupleIndex { object, index } => {
                self.compile_tuple_index(object, *index as usize)
            }
            ExprKind::Cast { expr: inner, ty } => {
                // Compute the source-type signedness BEFORE compiling inner —
                // `expr_is_unsigned_int` is a pure structural inspection (no
                // state writes) so ordering doesn't matter for correctness,
                // but reading the inner's shape before lowering keeps the
                // dependency direction obvious. Drives sext vs zext in
                // `compile_cast`'s widening lane.
                let source_is_unsigned = self.expr_is_unsigned_int(inner);
                // Target signedness drives `fptoui.sat` vs `fptosi.sat` for the
                // float→int (saturating) lane — read from the target type name.
                let target_is_unsigned = matches!(&ty.kind, TypeKind::Path(p)
                if matches!(
                    p.segments.first().map(|s| s.as_str()),
                    Some("u8") | Some("u16") | Some("u32") | Some("u64") | Some("u128") | Some("usize")
                ));
                let val = self.compile_expr(inner)?;
                let target_ty = self.llvm_type_for_type_expr(ty);
                let casted =
                    self.compile_cast(val, target_ty, source_is_unsigned, target_is_unsigned)?;
                // `x as Refined` enforces the refinement predicate at runtime
                // (phase-9 step 5c). The cast value is already the base
                // layout; a false predicate aborts with a contract fault.
                if let TypeKind::Path(p) = &ty.kind {
                    if let Some(name) = p.segments.first() {
                        if self.refinement_predicates.contains_key(name) {
                            let name = name.clone();
                            self.emit_refinement_assert(&name, casted)?;
                        }
                    }
                }
                Ok(casted)
            }
            ExprKind::Match { scrutinee, arms } => self.compile_match(scrutinee, arms),
            ExprKind::For {
                label,
                pattern,
                iterable,
                body,
                ..
            } => self.compile_for(label.as_deref(), pattern, iterable, body),
            ExprKind::IfLet {
                pattern,
                value,
                then_block,
                else_branch,
            } => self.compile_if_let(pattern, value, then_block, else_branch.as_deref()),
            // Unsafe blocks: safety checks live in earlier phases; codegen just
            // compiles the inner block normally.
            ExprKind::Unsafe(block) => match self.compile_block_with_frame(block)? {
                Some(v) => Ok(v),
                None => Ok(self.context.i64_type().const_int(0, false).into()),
            },
            ExprKind::Par(block) => self.compile_par_block(block),
            ExprKind::MethodCall {
                object,
                method,
                args,
                ..
            } => self.compile_method_call(object, method, args, &expr.span),
            ExprKind::Index { object, index } => self.compile_index(object, index),
            ExprKind::Question(inner) => self.compile_question(inner, &expr.span),
            ExprKind::Path { segments, .. } => self.compile_path_expr(segments, &expr.span),
            ExprKind::LabeledBlock { label, body, .. } => self.compile_labeled_block(label, body),
            ExprKind::OffsetOf { ty, field_path } => self.compile_offset_of(ty, field_path),
            ExprKind::Lock { mutex, alias, body } => {
                self.compile_lock_block(mutex, alias.as_deref(), body)
            }
            ExprKind::WhileLet {
                label,
                pattern,
                value,
                body,
                ..
            } => self.compile_while_let(label.as_deref(), pattern, value, body),
            _ => Ok(self.context.i64_type().const_int(0, false).into()),
        }
    }

    /// Lower `offset_of[T](field.path)` to a compile-time `usize` constant.
    /// The typechecker has already validated that `T` is a struct and the
    /// path is well-typed; here we walk the lowered LLVM struct types to
    /// chain `TargetData::offset_of_element` across nested-path segments.
    /// Returns the byte offset as an `i64` constant.
    pub(super) fn compile_offset_of(
        &mut self,
        ty: &TypeExpr,
        field_path: &[String],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let i64_ty = self.context.i64_type();
        // Recover the initial struct name from the type expression.
        let TypeKind::Path(path) = &ty.kind else {
            return Err("offset_of: target must be a path-named struct".to_string());
        };
        let mut current_struct_name = path
            .segments
            .last()
            .ok_or_else(|| "offset_of: empty type path".to_string())?
            .clone();
        let mut total_offset: u64 = 0;
        for segment_name in field_path {
            let struct_ty = self
                .struct_types
                .get(&current_struct_name)
                .copied()
                .ok_or_else(|| {
                    format!(
                        "offset_of: struct '{current_struct_name}' has no LLVM \
                         type registration"
                    )
                })?;
            let field_names = self
                .struct_field_names
                .get(&current_struct_name)
                .ok_or_else(|| {
                    format!(
                        "offset_of: struct '{current_struct_name}' has no field-name \
                         table"
                    )
                })?
                .clone();
            let field_idx = field_names
                .iter()
                .position(|n| n == segment_name)
                .ok_or_else(|| {
                    format!(
                        "offset_of: field '{segment_name}' not found on struct \
                         '{current_struct_name}'"
                    )
                })?;
            let target_data = self.ensure_target_data()?;
            let segment_offset = target_data
                .offset_of_element(&struct_ty, field_idx as u32)
                .ok_or_else(|| {
                    format!(
                        "offset_of: TargetData rejected element index {field_idx} \
                         on struct '{current_struct_name}'"
                    )
                })?;
            total_offset += segment_offset;
            // Chase the field's type for the next segment.
            let field_type_names = self
                .struct_field_type_names
                .get(&current_struct_name)
                .cloned();
            if let Some(ftns) = field_type_names {
                if let Some(Some(next_name)) = ftns.get(field_idx) {
                    current_struct_name = next_name.clone();
                }
            }
        }
        Ok(i64_ty.const_int(total_offset, false).into())
    }

    /// Compile a `Type.Variant` path expression. The parser emits `Color.Red`
    /// as `ExprKind::Path(["Color", "Red"])` (any dotted ident sequence whose
    /// segments start with an uppercase letter). The only case currently
    /// reaching this arm is unit-variant construction — payload-bearing
    /// variants go through `ExprKind::Call { callee: Path(...), args }` and
    /// are dispatched by `compile_assoc_call`.
    pub(super) fn compile_path_expr(
        &mut self,
        segments: &[String],
        span: &crate::token::Span,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        // `ExitCode.SUCCESS` / `ExitCode.FAILURE` (Phase-8 entry-point
        // contract Slice B). Parsed as a 2-segment `Path` (leading
        // segment is a known type name). `ExitCode` is `distinct type =
        // i32`, so the constant lowers to a bare `i32`. Mirrors the
        // typechecker / interpreter sibling Path intercepts; without
        // this the access falls through to the `i64 0` placeholder
        // below and `main() -> ExitCode { ExitCode.FAILURE }` exits 0.
        if segments.len() == 2 {
            if let Some(code) = crate::prelude::lookup_exitcode_const(&segments[0], &segments[1]) {
                return Ok(self.context.i32_type().const_int(code as u64, false).into());
            }
        }
        // Value-binding-rooted field path — `F.value` (local), `CFG.max`
        // (module binding), `OUTER.inner.field` (nested). The parser greedily
        // consumes an uppercase-led dotted chain into a single `Path`
        // (`src/parser/exprs.rs` — the `while self.eat(&Token::Dot)` loop), so
        // field reads on a value binding land here rather than in the
        // `FieldAccess` arm. Sibling of the typechecker `resolve_path_type`
        // walk and the interpreter Path intercept. Generalizes the slice-10
        // module-binding-only, 2-segment arm to local-variable roots and
        // nested paths by synthesizing the equivalent `Identifier`-rooted
        // nested `FieldAccess` chain and reusing `compile_expr` — which
        // already loads either root (the Identifier arm checks `variables`
        // then `module_bindings`) and extracts each field through the full
        // field-access machinery (struct / shared-struct / type recovery).
        // Without it, local and nested paths fell through to the `i64 0`
        // placeholder below and silently read 0. Guarded to value-binding
        // roots so enum-variant / unit-variant paths fall through unchanged.
        if segments.len() >= 2
            && (self.variables.contains_key(&segments[0])
                || self.module_bindings.contains_key(&segments[0]))
        {
            let mut obj = Expr {
                span: span.clone(),
                kind: ExprKind::Identifier(segments[0].clone()),
            };
            for member in &segments[1..] {
                obj = Expr {
                    span: span.clone(),
                    kind: ExprKind::FieldAccess {
                        object: Box::new(obj),
                        field: member.clone(),
                    },
                };
            }
            return self.compile_expr(&obj);
        }
        if segments.len() == 2 {
            let type_name = &segments[0];
            let variant_name = &segments[1];
            if let Some(layout) = self.enum_layouts.get(type_name).cloned() {
                if let Some(&tag) = layout.tags.get(variant_name) {
                    if layout.field_counts.get(variant_name).copied().unwrap_or(0) == 0 {
                        let i64_t = self.context.i64_type();
                        if let Some(info) = self.shared_types.get(type_name).cloned() {
                            let ptr = self.emit_rc_alloc(info.heap_type);
                            let tag_ptr = self
                                .builder
                                .build_struct_gep(info.heap_type, ptr, 1, "sh_tag")
                                .unwrap();
                            self.builder
                                .build_store(tag_ptr, i64_t.const_int(tag, false))
                                .unwrap();
                            return Ok(ptr.into());
                        }
                        // Zero-init so a multi-word enum's unit variant has `0`
                        // payload words (not undef) — `V::B == V::B` folded to
                        // undef under the word-wise `==` otherwise.
                        let mut agg = layout.llvm_type.const_zero();
                        agg = self
                            .builder
                            .build_insert_value(agg, i64_t.const_int(tag, false), 0, "tag")
                            .unwrap()
                            .into_struct_value();
                        return Ok(agg.into());
                    }
                }
            }
        }
        Ok(self.context.i64_type().const_int(0, false).into())
    }

    /// Compile the `?` early-propagation operator for `Result[T,E]` and `Option[T]`.
    ///
    /// The operand is a `{ i64 tag, i64 w0 }` enum struct. Tag semantics:
    ///   Result: Err=0, Ok=1
    ///   Option: None=0, Some=1
    ///
    /// On failure (tag == 0): early-return `{ 0, w0 }` from the current function,
    /// propagating the error/None payload unchanged.
    /// On success (tag == 1): yield `w0` (the unwrapped value) and continue.
    pub(super) fn compile_question(
        &mut self,
        inner: &Expr,
        outer_span: &crate::token::Span,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let val = self.compile_expr(inner)?;
        // `r?` consumes `r` — disarm its scope-exit inline-payload free
        // (registered by the B-2026-06-10-6 Option/Result work) now that the
        // Result/Option VALUE has been captured into `val`; otherwise the
        // source double-frees the payload the unwrap binding (Ok) or the
        // caller (Err) takes ownership of. No-op for temp / non-inline operands.
        self.suppress_question_source_inline_payload(inner);
        let i64_t = self.context.i64_type();
        // The early-return struct must match the enclosing function's
        // declared LLVM return type. Result keeps the legacy
        // `{i64, i64}` layout; Option was widened to
        // `{i64, i64, i64, i64}` to fit multi-word payloads (tuple,
        // Vec, String). Pull the actual return type from the function
        // declaration instead of hardcoding a narrow shape.
        let fn_val = self.current_fn.unwrap();
        let enum_ty = match fn_val.get_type().get_return_type() {
            Some(BasicTypeEnum::StructType(s)) => s,
            _ => self.context.struct_type(
                &[BasicTypeEnum::IntType(i64_t), BasicTypeEnum::IntType(i64_t)],
                false,
            ),
        };

        // Extract tag (field 0) and payload word (field 1)
        let tag = self
            .builder
            .build_extract_value(val.into_struct_value(), 0, "q_tag")
            .unwrap()
            .into_int_value();
        let w0 = self
            .builder
            .build_extract_value(val.into_struct_value(), 1, "q_w0")
            .unwrap();

        // Check tag == 0 (failure path)
        let is_failure = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::EQ,
                tag,
                i64_t.const_int(0, false),
                "q_is_fail",
            )
            .unwrap();

        let cur_fn = self
            .builder
            .get_insert_block()
            .unwrap()
            .get_parent()
            .unwrap();
        let fail_bb = self.context.append_basic_block(cur_fn, "q_fail");
        let ok_bb = self.context.append_basic_block(cur_fn, "q_ok");

        self.builder
            .build_conditional_branch(is_failure, fail_bb, ok_bb)
            .unwrap();

        // Failure block: push an error-return-trace frame, drain scope
        // cleanup actions, optionally convert the err payload via
        // `Target.from(e)`, build `{ 0, w0' }`, and early-return.
        // The cleanup walks the full `scope_cleanup_actions` stack so any
        // heap-owning bindings live at this `?` site (Vec/String buffers, RC
        // values, Map handles) are released before the function returns.
        // The trace push happens BEFORE cleanup so the runtime sees the
        // failure site in source order even if cleanup itself crashes.
        //
        // Slice 2 (Phase 7 § *defer / errdefer codegen*): route through
        // `emit_scope_cleanup_for_error_path`, which runs `UserErrDefer`
        // bodies in phase 1 (LIFO across frames) before the regular
        // drop+defer drain. The `?` failure branch is the canonical
        // error-exit path: an `errdefer { ... }` registered upstream of
        // the `?` site fires here exactly as the interpreter's
        // `run_cleanup` on `ExitPath::Err` would.
        self.builder.position_at_end(fail_bb);
        // `?`-error-return-trace instrumentation — a debug-only diagnostic,
        // elided in a release build (`strip_error_trace`) for zero `?`-site cost.
        if !self.strip_error_trace {
            self.emit_error_trace_push(outer_span);
        }
        // Slice 4 (Phase 7 § *defer / errdefer codegen*): stage the
        // about-to-be-returned Err payload so any in-scope binding-form
        // errdefer (`errdefer(e) { ... }`) can bind `e` during its
        // phase-1 emission inside `emit_scope_cleanup_for_error_path`.
        //
        // Slice 4 follow-up (a) — wider-E payload reconstruction
        // (2026-05-26). When `current_fn_err_payload_ty` is set (the
        // current function returns `Result[T, E]` and codegen knows
        // E's LLVM type from the annotation), extract every payload
        // word the result struct carries (w0/w1/w2 at fields 1/2/3 —
        // synthesizing 0 for words past the struct's count_fields) and
        // call `rebuild_value_from_payload_words(E_ty, w0, w1, w2)` to
        // get the source-typed Err value. Stage that. The helper
        // handles primitives (i8..i64 truncate / zext), floats
        // (bitcast), pointers (inttoptr), Vec/String 3-word struct,
        // Slice 2-word struct, and generic user structs field-by-field.
        // Pre-(a), the `?` site staged bare `w0` as i64 — the binding
        // `e: String` saw the data-ptr-as-i64 (garbage from the
        // binding's perspective). When `current_fn_err_payload_ty` is
        // `None` (no annotation / not a `Result[T, E]` return type),
        // fall back to staging `w0` as before.
        let staged_payload = match self.current_fn_err_payload_ty {
            Some(e_ty) => {
                let w0_i = w0.into_int_value();
                let payload_word_count = enum_ty.count_fields().saturating_sub(1) as usize;
                let zero = i64_t.const_int(0, false);
                let w1_i = if payload_word_count >= 2 {
                    self.builder
                        .build_extract_value(val.into_struct_value(), 2, "q_w1")
                        .unwrap()
                        .into_int_value()
                } else {
                    zero
                };
                let w2_i = if payload_word_count >= 3 {
                    self.builder
                        .build_extract_value(val.into_struct_value(), 3, "q_w2")
                        .unwrap()
                        .into_int_value()
                } else {
                    zero
                };
                self.rebuild_value_from_payload_words(e_ty, w0_i, w1_i, w2_i)
                    .ok()
            }
            None => Some(w0),
        };
        self.pending_errdefer_payload = staged_payload;
        self.emit_scope_cleanup_for_error_path();
        self.pending_errdefer_payload = None;

        // Cross-error-type conversion: when the typechecker recorded a target
        // type for this `?` site, look up the LLVM function `Target.from` and
        // call it on the inner err payload. The user-impl `T.from` LLVM
        // function is already compiled by the impl-block pass.
        let key = (outer_span.offset, outer_span.length);
        let propagated_payload: BasicValueEnum<'ctx> =
            if let Some(target) = self.question_conversions.get(&key).cloned() {
                let qualified = format!("{}.from", target);
                if let Some(from_fn) = self.module.get_function(&qualified) {
                    // The inner err payload was unpacked into the uniform
                    // i64 word `w0` by the enum-payload codegen, but
                    // `Target.from(e: SourceError)` is declared at the
                    // surface level taking the error type itself — for any
                    // `struct SourceError { ... }` LLVM lowers that to the
                    // struct shape. Reconstitute the struct value from the
                    // i64 word so the call's argument matches the param
                    // type. Single-field structs (the common error-wrapper
                    // shape) take field 0 from `w0`; other shapes pass `w0`
                    // through unchanged (the typechecker rejects these
                    // before reaching codegen, so this is just a safety
                    // fallback).
                    let arg_ty = from_fn.get_nth_param(0).unwrap().get_type();
                    let arg: BasicValueEnum<'ctx> = match arg_ty {
                        BasicTypeEnum::StructType(st) if st.count_fields() == 1 => {
                            let undef = st.get_undef();
                            self.builder
                                .build_insert_value(undef, w0, 0, "q_from_arg")
                                .unwrap()
                                .into_struct_value()
                                .into()
                        }
                        _ => w0,
                    };
                    let call_site = self
                        .builder
                        .build_call(from_fn, &[arg.into()], "q_from")
                        .unwrap();
                    call_site.try_as_basic_value().unwrap_basic()
                } else {
                    // No matching impl emitted — propagate raw payload.
                    // The typechecker should have rejected this case; staying
                    // permissive keeps codegen non-fatal on unexpected inputs.
                    w0
                }
            } else {
                w0
            };

        // The error-payload slot is a uniform i64 word (matches the
        // tag+i64-words enum lowering). User-impl `Target.from(e)` returns
        // the target type's value — a struct for any `struct MyError { ... }`.
        // Coerce so `insertvalue` agrees with the slot's element type;
        // single-field structs (the common error-wrapper shape) extract to
        // their inner field.
        let propagated_word = self.coerce_to_i64(propagated_payload)?;

        if self.current_fn_name == "main" && self.main_result_err_te.is_some() {
            // `?` error propagation inside `main() -> Result[(), E]`: `main`'s
            // LLVM signature is the C entry `i32`, so we cannot `ret` the
            // `{tag, …}` Err aggregate (verify failure — B-2026-06-12-9).
            // Instead reconstruct the source-typed error from the propagated
            // payload word and emit the design.md § Entry Point error exit
            // (`Error: {e}\n` to stderr, exit 1). Single-word E reconstructs
            // exactly; a multi-word E sees only w0 here — the same documented
            // limitation as the wider-E `?` Err-binding path above.
            let err_val = match self.main_result_err_te.clone() {
                Some(te) => {
                    let e_ty = self.llvm_type_for_type_expr(&te);
                    let zero = i64_t.const_int(0, false);
                    self.rebuild_value_from_payload_words(e_ty, propagated_word, zero, zero)
                        .unwrap_or_else(|_| propagated_word.into())
                }
                None => propagated_word.into(),
            };
            self.emit_main_result_err_exit(err_val);
        } else if self.current_fn_ret_is_niche() {
            // Niche-ABI enclosing fn (`-> Option[shared T]` declared as
            // a nullable ptr): the `?` failure path early-returns None,
            // which is null under the niche. No struct to build.
            let null = self
                .context
                .ptr_type(inkwell::AddressSpace::default())
                .const_null();
            self.builder.build_return(Some(&null)).unwrap();
        } else {
            let ret_struct = {
                let undef = enum_ty.get_undef();
                let s1 = self
                    .builder
                    .build_insert_value(undef, i64_t.const_int(0, false), 0, "q_ret_tag")
                    .unwrap();
                self.builder
                    .build_insert_value(s1, propagated_word, 1, "q_ret_val")
                    .unwrap()
            };
            self.builder.build_return(Some(&ret_struct)).unwrap();
        }

        // Ok/Some block: clear any frames a recovered earlier `?` had
        // pushed, then continue with the unwrapped payload word. Mirrors
        // the interpreter's `clear_error_trace` call on the success path
        // (src/interpreter.rs:1501).
        self.builder.position_at_end(ok_bb);
        // Paired with the push above — also elided under `strip_error_trace`.
        if !self.strip_error_trace {
            self.builder
                .build_call(self.karac_error_trace_clear_fn, &[], "q_trace_clear")
                .unwrap();
        }
        // Reconstruct a multi-word Ok/Some payload from all its words. The
        // success path historically returned only `w0` (the first payload
        // word), silently truncating a 3-word `Vec`/`String` (losing
        // `len`/`cap`) → malformed value that crashes on use. Surfaced by
        // `Result[Vec[T], AllocError]?` from the fallible constructors, but the
        // bug is general (any multi-word `?` payload — `String`, tuples).
        self.reconstruct_question_ok_payload(inner, val, w0)
    }

    /// Rebuild the `Ok`/`Some` payload value at a `?` success path. `?`
    /// originally returned only the first payload word `w0`; a multi-word
    /// payload (3-word `Vec`/`String`, 2-word `Slice`, small struct) needs all
    /// its words or the value is malformed (missing `len`/`cap` → crash on
    /// first use). Recover the operand's `Ok`/`Some` element type from its
    /// recorded generic instantiation (`enum_inst_type_exprs`, e.g.
    /// `Result[Vec[i64], AllocError]` / `Result[String, AllocError]`) and
    /// rebuild from the payload words via `rebuild_value_from_payload_words`.
    /// A scalar / pointer / float payload — or any operand whose type wasn't
    /// recorded — keeps the single-word `w0`, which is exactly its value.
    fn reconstruct_question_ok_payload(
        &mut self,
        inner: &Expr,
        result_val: BasicValueEnum<'ctx>,
        w0: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        // The `?` operand and the `?` expression share a span (parser
        // collision), and the `?` result type — i.e. the *unwrapped* Ok/Some
        // payload type — is the last write at that key. So the recorded type
        // at the operand span IS the payload type directly:
        //   * `String` lands in `string_typed_exprs` (`Type::Str` isn't a
        //     `Named`, so `enum_inst_type_exprs` misses it); its layout is the
        //     3-word `{ptr, len, cap}` vec shape.
        //   * `Vec[T]` and other generic `Named` payloads land in
        //     `enum_inst_type_exprs`, recorded as the payload type itself.
        let key = (inner.span.offset, inner.span.length);
        let payload_llvm: BasicTypeEnum<'ctx> = if self.string_typed_exprs.contains(&key) {
            self.vec_struct_type().into()
        } else if let Some(te) = self.enum_inst_type_exprs.get(&key).cloned() {
            // Defensive: if the `Result`/`Option` wrapper itself were recorded
            // here, its enum aggregate must not be rebuilt from 3 payload words.
            if let TypeKind::Path(p) = &te.kind {
                if matches!(
                    p.segments.first().map(String::as_str),
                    Some("Result") | Some("Option")
                ) {
                    return Ok(w0);
                }
            }
            self.llvm_type_for_type_expr(&te)
        } else {
            return Ok(w0);
        };
        // Only multi-word struct payloads (Vec / String / Slice / small
        // struct) need reconstruction; scalars / pointers / floats are
        // exactly `w0`.
        if !matches!(payload_llvm, BasicTypeEnum::StructType(_)) {
            return Ok(w0);
        }
        let ok_llvm = payload_llvm;
        let sv = result_val.into_struct_value();
        let n_fields = sv.get_type().count_fields();
        let i64_t = self.context.i64_type();
        let zero = i64_t.const_int(0, false);
        let w0_i = w0.into_int_value();
        let w1_i = if n_fields >= 3 {
            self.builder
                .build_extract_value(sv, 2, "q_ok_w1")
                .unwrap()
                .into_int_value()
        } else {
            zero
        };
        let w2_i = if n_fields >= 4 {
            self.builder
                .build_extract_value(sv, 3, "q_ok_w2")
                .unwrap()
                .into_int_value()
        } else {
            zero
        };
        self.rebuild_value_from_payload_words(ok_llvm, w0_i, w1_i, w2_i)
    }

    /// Slice 2 (Phase 7 § *defer / errdefer codegen*). Recognise the
    /// syntactic shape of an error-exit return value so the surrounding
    /// `return` / function-tail emitter can route cleanup through
    /// `emit_scope_cleanup_for_error_path`. Matches `Err(...)`,
    /// `Result.Err(...)`, `None`, and `Option.None` — the four error-path
    /// shapes a Result-returning or Option-returning function can produce
    /// at a `return` site or as a tail expression. `Ok(...)` / `Some(...)`
    /// / plain values are normal-exit shapes and take the regular
    /// drop+defer drain. Detection is purely syntactic: the typechecker
    /// has already gated where errdefer is legal (Result/Option-returning
    /// functions) and where these variant constructors can appear, so
    /// inspecting the call's path segments is sufficient.
    pub(super) fn is_error_exit_value(expr: &Expr) -> bool {
        fn last_segment_is(segments: &[String], name: &str) -> bool {
            segments.last().is_some_and(|s| s == name)
        }
        match &expr.kind {
            ExprKind::Call { callee, .. } => match &callee.kind {
                ExprKind::Path { segments, .. } => last_segment_is(segments, "Err"),
                ExprKind::Identifier(name) => name == "Err",
                _ => false,
            },
            ExprKind::Path { segments, .. } => last_segment_is(segments, "None"),
            ExprKind::Identifier(name) => name == "None",
            _ => false,
        }
    }

    /// Materialize a string literal's bytes as an internal, NUL-terminated
    /// constant byte-array global and return a pointer to its data. Unlike
    /// `build_global_string_ptr` (which lowers to `LLVMBuildGlobalString` and
    /// treats the value as a C string — truncating at the first interior
    /// NUL), `const_string` preserves interior NUL bytes, so a length-prefixed
    /// String literal like `"a\0b"` carries all its bytes through to the
    /// `{ptr,len,cap}` value and the `len`-bounded `fwrite`/memcpy that read
    /// it (L5). The trailing NUL terminator is harmless: the String's `len`
    /// excludes it and `cap = 0` means the global is never freed, while
    /// NUL-free literals remain valid C strings for any FFI that wants one.
    pub(super) fn build_str_bytes_global(
        &self,
        bytes: &[u8],
        name: &str,
    ) -> inkwell::values::PointerValue<'ctx> {
        let i8_ty = self.context.i8_type();
        let arr_ty = i8_ty.array_type(bytes.len() as u32 + 1); // +1 trailing NUL
        let data = self.context.const_string(bytes, true);
        let g = self.module.add_global(arr_ty, None, name);
        g.set_initializer(&data);
        g.set_constant(true);
        g.set_linkage(inkwell::module::Linkage::Internal);
        g.as_pointer_value()
    }

    /// Slice 4 (Phase 7 § *defer / errdefer codegen*). For a syntactic
    /// `Err(arg)` expression, return the inner `arg` so the caller can
    /// re-compile it to obtain the source-typed payload value for an
    /// `errdefer(e) { ... }` binding. Returns `None` for `None` and any
    /// other shape (including no-arg `Err`, which is an arity error the
    /// typechecker rejects before reaching codegen). Mirrors the call-
    /// recognition gate used by `is_error_exit_value`: both `Err`-as-
    /// identifier and `Path([..., "Err"])` callee shapes are accepted.
    /// Slice 4 follow-up (b) — double-eval fix (2026-05-26). True
    /// when `expr` is a syntactic shape whose `compile_expr` lowering
    /// is observably side-effect-free in source semantics — re-
    /// evaluating it yields the same value with no observable extra
    /// behaviour. Used by the `ExprKind::Return(Err(arg))` and
    /// function-tail `Err(arg)` emitters to decide whether to re-
    /// compile the payload expression for binding-form errdefer
    /// staging (preserves wider-E source-typed binding for pure
    /// args) or extract the i64-coerced payload word from the
    /// already-constructed Err return struct (single eval for
    /// impure args, at the cost of wider-E precision).
    ///
    /// Whitelist: `Identifier`, `Path`, integer / float / bool /
    /// char / byte / string literals. Identifier/Path re-compile
    /// to a load from a local alloca or global, which is value-
    /// stable across two reads at the same program point (no
    /// intervening write at the same callsite). Literals are
    /// constants. `StringLit` materialises a global string ptr
    /// once and reuses it on subsequent compile_expr calls (per
    /// `compile_expr`'s `StringLit` arm), so re-compile produces
    /// the same `{ptr, len, cap}` struct value with `cap=0` —
    /// safe to re-emit.
    ///
    /// Out of whitelist: any `Call` / `MethodCall` / `FieldAccess`
    /// / `Index` / `Unary` / `Binary` (operators lower to method
    /// calls via `lowering.rs`'s desugaring pass) / `Block` / etc.
    /// Conservative — false negatives mean we drop into the
    /// extract-from-v path, accepting the i64-coerce trade for
    /// wider-E payloads. Adding more shapes to the whitelist (e.g.
    /// `Binary` over pure operands) is fine but unnecessary for
    /// the common Err-arg shapes seen in practice (`Err(literal)`,
    /// `Err(error_code)`, `Err(name.into())`).
    pub(super) fn is_pure_recompilable(expr: &Expr) -> bool {
        matches!(
            &expr.kind,
            ExprKind::Identifier(_)
                | ExprKind::Path { .. }
                | ExprKind::Integer(_, _)
                | ExprKind::Float(_, _)
                | ExprKind::Bool(_)
                | ExprKind::CharLit(_)
                | ExprKind::ByteLit(_)
                | ExprKind::StringLit(_)
                | ExprKind::SelfValue
                | ExprKind::SelfType
        )
    }

    pub(super) fn err_payload_from_value(expr: &Expr) -> Option<&Expr> {
        if let ExprKind::Call { callee, args } = &expr.kind {
            let is_err_ctor = match &callee.kind {
                ExprKind::Path { segments, .. } => segments.last().is_some_and(|s| s == "Err"),
                ExprKind::Identifier(name) => name == "Err",
                _ => false,
            };
            if is_err_ctor {
                if let Some(arg) = args.first() {
                    return Some(&arg.value);
                }
            }
        }
        None
    }

    /// Emit a call to `karac_error_trace_push(file, file_len, line, col)`
    /// at the current insertion point. When `source_filename` is set, a
    /// deduped global string is materialized on first call and reused for
    /// every subsequent `?` site in the module — runtime-side, the printer
    /// formats `<file>:<line>:<col>` rows. When unset, file=null/len=0 and
    /// the runtime prints `<line>:<col>` only (one .kara file at a time).
    pub(super) fn emit_error_trace_push(&mut self, outer_span: &crate::token::Span) {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_ty = self.context.i64_type();
        let i32_ty = self.context.i32_type();
        let (file_ptr, file_len_val) = match self.ensure_source_filename_global() {
            Some((p, len)) => (p, i64_ty.const_int(len, false)),
            None => (ptr_ty.const_null(), i64_ty.const_int(0, false)),
        };
        let line = i32_ty.const_int(outer_span.line as u64, false);
        let col = i32_ty.const_int(outer_span.column as u64, false);
        self.builder
            .build_call(
                self.karac_error_trace_push_fn,
                &[
                    file_ptr.into(),
                    file_len_val.into(),
                    line.into(),
                    col.into(),
                ],
                "q_trace_push",
            )
            .unwrap();
    }

    /// Lazily materialize the LLVM global string for `source_filename` and
    /// return its `(ptr, byte_len)`. Returns `None` when no filename was
    /// threaded in. The byte length is the source filename's byte length —
    /// the runtime's printer writes that many bytes verbatim, so the
    /// trailing NUL added by `build_global_string_ptr` is intentionally
    /// excluded.
    pub(super) fn ensure_source_filename_global(&mut self) -> Option<(PointerValue<'ctx>, u64)> {
        if let Some(cached) = self.source_filename_global {
            return Some(cached);
        }
        let name = self.source_filename.as_ref()?.clone();
        let len = name.len() as u64;
        let global = self
            .builder
            .build_global_string_ptr(&name, "karac.source_filename")
            .unwrap();
        let ptr = global.as_pointer_value();
        self.source_filename_global = Some((ptr, len));
        Some((ptr, len))
    }

    // ── Struct/tuple expressions ──────────────────────────────────

    /// Compile a borrowed-struct `ref` field initializer to the borrow
    /// POINTER stored in the field slot (which lowers to `ptr`). Mirrors
    /// ref-parameter argument passing (`calls.rs`): an identifier forwards its
    /// data pointer (a `ref` param's stored borrow, or an owned binding's
    /// address); an indexed place yields the element pointer; any other rvalue
    /// is materialized to a temporary whose address is taken. design.md
    /// Feature 4 Part 3 (borrowed structs); B-2026-06-07-5.
    fn compile_ref_field_borrow_ptr(
        &mut self,
        value: &Expr,
        idx: usize,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        if let ExprKind::Identifier(var_name) = &value.kind {
            if let Some(ptr) = self.get_data_ptr(var_name) {
                return Ok(ptr.into());
            }
        }
        if let Some(elem_ptr) = self.ref_arg_index_borrow_ptr(value)? {
            return Ok(elem_ptr.into());
        }
        let v = self.compile_expr(value)?;
        Ok(self.materialize_rvalue_for_ref_arg(v, idx))
    }

    pub(super) fn compile_struct_init(
        &mut self,
        name: &str,
        fields: &[FieldInit],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        // FFI union literal (phase 5 line 569 slice 4). The typechecker
        // already enforces exactly-one-field shape (`E_UNION_LITERAL_REQUIRES_ONE_FIELD`,
        // slice 2c), absent spread, and field-type matching, so codegen
        // can assume `fields.len() == 1` and the field name resolves to
        // a known union member. The storage layout is the
        // max-alignment-first struct seeded by `declare_unions` —
        // `union_types[name]` for the aggregate and `union_field_types[name]`
        // for the per-field destination LLVM type.
        //
        // Lowering: alloca the storage type, untyped-store the field
        // value at offset 0 (LLVM opaque pointers; the store writes the
        // field's natural width, leaving any storage padding undef —
        // matches Rust union-literal semantics where only the active
        // variant's bytes are initialized), then load the storage type
        // back so callers receive an SSA `StructValue` of the union's
        // storage shape ready for the let-stmt's alloca-and-store path.
        if let Some(&storage_ty) = self.union_types.get(name) {
            return self.compile_union_init(name, storage_ty, fields);
        }
        // Shared struct: heap-allocate with refcount header — unless
        // phase-D headerless layout applies to this (fn, type), in
        // which case the rc word is omitted entirely (no header slot,
        // no rc=1 store) and field GEPs use the twin type at base 0.
        if let Some(info) = self.shared_types.get(name).cloned() {
            if !info.is_enum {
                let (gep_ty, base) = self.shared_gep_layout(name, info.heap_type);
                let ptr = if base == 0 {
                    self.emit_headerless_alloc(gep_ty)
                } else {
                    self.emit_rc_alloc(info.heap_type)
                };
                for (idx, field_init) in fields.iter().enumerate() {
                    let val = self.compile_expr(&field_init.value)?;
                    // Owned String/Vec PARAM captured into a field
                    // (`Node { name: s }` where `s: String` is a param):
                    // deep-copy — the caller retains the buffer's free
                    // under the by-value header ABI (kata-22 family).
                    let val = self.maybe_defensive_copy_param_arg(&field_init.value, val);
                    // Fields start at index `base` (0 headerless;
                    // 1 headered — index 0 is the refcount).
                    let field_ptr = self
                        .builder
                        .build_struct_gep(
                            gep_ty,
                            ptr,
                            idx as u32 + base,
                            &format!("field_{}", field_init.name),
                        )
                        .unwrap();
                    // Niche-opt: the field slot is a single `ptr`, not the
                    // 4-i64 Option enum. Extract w0 from the freshly-
                    // computed Option value and store as ptr.
                    let niche_inner = self.niche_field_inner_heap_type(name, idx);
                    // Resolve the field's inner shared heap type if this is
                    // an `Option[shared T]` field (niche-opt or conventional).
                    let opt_inner_heap = niche_inner.or_else(|| {
                        self.struct_field_type_exprs
                            .get(name)
                            .and_then(|tes| tes.get(idx))
                            .cloned()
                            .and_then(|te| self.option_inner_shared_type_for_type_expr(&te))
                            .map(|(_, info)| info.heap_type)
                    });
                    if niche_inner.is_some() {
                        self.niche_store_option_field(field_ptr, val);
                    } else {
                        // Width coercion at the field-init boundary —
                        // a default-width literal against a narrower
                        // declared field would store 8 bytes over the
                        // narrow slot, corrupting neighboring fields.
                        // See `coerce_to_struct_field_ty`.
                        let val = self.coerce_to_struct_field_ty(gep_ty, idx as u32 + base, val);
                        self.builder.build_store(field_ptr, val).unwrap();
                    }
                    // Capture-inc for a non-fresh `Option[shared T]` field
                    // value: the new struct holds an independent ref to that
                    // chain. `suppress_source_vec_cleanup_for_arg` below only
                    // transfer-inc's a bare `shared struct` field value (its
                    // shared_types lookup misses an `Option[shared]` binding
                    // like a param), so without this an `Option[shared]`
                    // capture is uncounted → over-dec on return (kata #19).
                    // Fresh values (`Some(node)`, call move-out) already own
                    // their ref — skip via `rhs_yields_fresh_ref`. A literal
                    // `None` init has no inner to count: the inc it used to
                    // emit was a dead tag-guarded branch (None's w0 is undef,
                    // tag is constantly 0) — folded away at O2 but noise in
                    // the IR and in count-free pins; skip it outright.
                    let init_is_none =
                        matches!(&field_init.value.kind, ExprKind::Identifier(n) if n == "None");
                    if let Some(inner_heap) = opt_inner_heap {
                        if !init_is_none && !self.rhs_yields_fresh_ref(&field_init.value) {
                            self.emit_rc_inc_for_captured_option(val, inner_heap);
                        }
                    }
                    // Move-aware suppression for `Foo { ..., body: body }`
                    // when the field expr is an Identifier naming a
                    // tracked Vec / String. The struct field now owns
                    // the heap buffer; without this, the source's
                    // scope-exit `FreeVecBuffer` frees the buffer the
                    // struct value carries downstream, producing UAF
                    // when the consumer reads through the struct.
                    // Mirrors the enum-variant constructor pattern
                    // already wired at `try_compile_enum_variant`.
                    self.suppress_source_vec_cleanup_for_arg(&field_init.value);
                    // Boxed / inline-heap `Option`/`Result` binding moved whole
                    // into this shared-struct field — see the non-shared peer
                    // below for the rationale.
                    self.suppress_inline_option_result_binding_move(&field_init.value);
                    // Map/Set sibling of the Vec suppression: a `Map`/`Set`
                    // local moved into this field hands the handle to the
                    // struct, so drop the source's scope-exit `FreeMapHandle`
                    // — otherwise the source frees the handle the struct now
                    // carries downstream (UAF / double-free when the consumer
                    // reads the field; Set/Map share `FreeMapHandle`).
                    if let ExprKind::Identifier(n) = &field_init.value.kind {
                        let n = n.clone();
                        self.suppress_map_cleanup_for_tail_identifier(&n);
                    }
                    self.suppress_fstr_acc_if_moved_out(&field_init.value);
                }
                return Ok(ptr.into());
            }
        }
        // Non-shared struct: stack-allocated aggregate.
        if let Some(&st) = self.struct_types.get(name) {
            let mut agg = st.get_undef();
            for (idx, field_init) in fields.iter().enumerate() {
                // Borrowed-struct `ref` field (design.md Feature 4 Part 3):
                // the field slot lowers to `ptr` and stores the BORROW
                // pointer, not the dereferenced value. `get_data_ptr`
                // forwards a `ref` param's stored borrow and takes an owned
                // binding's address — exactly ref-parameter argument passing.
                // No move-suppression / defensive-copy: a borrow neither owns
                // nor moves its source (the source keeps its drop; the field
                // carries only a pointer). The field-init order is the
                // declaration order (same assumption the `idx`-keyed insert
                // below already relies on), so `idx` indexes the declared
                // field types.
                let is_ref_field = self
                    .struct_field_type_exprs
                    .get(name)
                    .and_then(|tes| tes.get(idx))
                    .is_some_and(|te| matches!(te.kind, TypeKind::Ref(_) | TypeKind::MutRef(_)));
                if is_ref_field {
                    let ptr = self.compile_ref_field_borrow_ptr(&field_init.value, idx)?;
                    agg = self
                        .builder
                        .build_insert_value(agg, ptr, idx as u32, "ref_field")
                        .unwrap()
                        .into_struct_value();
                    continue;
                }
                let val = self.compile_expr(&field_init.value)?;
                // Owned String/Vec PARAM captured into a field — deep-copy,
                // same rationale as the shared-struct branch above.
                let val = self.maybe_defensive_copy_param_arg(&field_init.value, val);
                // Width coercion at the field-init boundary — inserting
                // a default-width literal into a narrower member builds
                // a malformed aggregate that reads back as garbage. See
                // `coerce_to_struct_field_ty`.
                let val = self.coerce_to_struct_field_ty(st, idx as u32, val);
                agg = self
                    .builder
                    .build_insert_value(agg, val, idx as u32, "field")
                    .unwrap()
                    .into_struct_value();
                // Capture-inc for a non-fresh `Option[shared T]` field value —
                // the non-shared peer of the shared-struct branch's inc above.
                // An owned (non-`shared`) struct still carries an `Option[shared]`
                // field by value (4-i64 conventional layout — niche-opt is a
                // `shared`-only layout, so no niche path here). When the field
                // value is an aliasing source (a local `tail` holding `Some(e)`,
                // a param), the new struct becomes an independent owner of that
                // inner chain and must inc; the source's own scope-exit
                // `FreeInlineOptionPayload` dec then balances back to the
                // construction-time count. Without it, `Block { tail: tail }`
                // returned from a builder fn hands the caller an under-counted
                // inner `Expr`, freed at end-of-builder-scope before the caller
                // reads it (#48 — the self-hosted parser's value-struct `Block`
                // tail SIGSEGV). Fresh values (`Some(node)`, a call move-out)
                // already own their ref — skipped via `rhs_yields_fresh_ref`;
                // a literal `None` has no inner to count.
                let opt_inner_heap = self
                    .struct_field_type_exprs
                    .get(name)
                    .and_then(|tes| tes.get(idx))
                    .cloned()
                    .and_then(|te| self.option_inner_shared_type_for_type_expr(&te))
                    .map(|(_, info)| info.heap_type);
                if let Some(inner_heap) = opt_inner_heap {
                    let init_is_none = matches!(
                        &field_init.value.kind,
                        ExprKind::Identifier(n) if n == "None"
                    );
                    if !init_is_none && !self.rhs_yields_fresh_ref(&field_init.value) {
                        self.emit_rc_inc_for_captured_option(val, inner_heap);
                    }
                }
                // Move-aware suppression — same shape as the shared-
                // struct branch above. The new struct aggregate carries
                // the source's data pointer; suppress the source's
                // scope-exit free so the consumer can read through.
                self.suppress_source_vec_cleanup_for_arg(&field_init.value);
                // #14 — field-access peer: `S { f: obj.field }` moves a heap
                // FIELD out of a tracked struct `obj`; cap-zero it so `obj`'s
                // StructDrop skips it (the new literal is the sole owner). The
                // whole-Identifier suppress above doesn't reach a FieldAccess.
                self.suppress_struct_field_move_into_literal(&field_init.value);
                // Boxed / inline-heap `Option`/`Result` binding moved whole into
                // this field: the field now owns the box / inline buffer, so
                // neutralize the source binding's `FreeInlineOptionPayload` /
                // `FreeInlineResultPayload` (which the `Vec`/`Map`/`fstr`
                // suppressors above don't cover). Without it, `TraitMethodNode {
                // body, .. }` for `let mut body = Some(parse_block())` frees the
                // boxed `Block` at the builder's scope exit while the returned
                // node still references it → UAF (selfhost slice 3c-iv).
                self.suppress_inline_option_result_binding_move(&field_init.value);
                // Map/Set sibling of the Vec suppression (see the shared-struct
                // branch above): a moved-in `Map`/`Set` local's source free is
                // dropped so the struct's owner is the sole freer.
                if let ExprKind::Identifier(n) = &field_init.value.kind {
                    let n = n.clone();
                    self.suppress_map_cleanup_for_tail_identifier(&n);
                }
                self.suppress_fstr_acc_if_moved_out(&field_init.value);
            }
            Ok(agg.into())
        } else {
            Ok(self.context.i64_type().const_int(0, false).into())
        }
    }

    /// Suppress the scope-exit free of an f-string accumulator whose
    /// buffer has just been moved out of the current scope — into a
    /// struct-literal field (`Foo { body: f"..." }`) or an explicit
    /// `return f"..."`. A direct f-string value stages `last_fstr_acc`
    /// during its `compile_expr`; the loaded `{data,len,cap}` String value
    /// is copied into the destination (which now owns the buffer), but the
    /// accumulator alloca still has a queued `FreeVecBuffer` cleanup. Take
    /// the staged acc and zero its `cap` so that cleanup's `cap > 0` guard
    /// no-ops — otherwise the accumulator frees the buffer the destination
    /// (and any downstream consumer / the destination's own drop) carries,
    /// a double-free that aborts under macOS malloc (exit 133). Mirrors the
    /// Let / Assign take points (`stmts.rs`) and the tail-return
    /// suppression (`compile_function`); the Identifier-named cases
    /// (`Foo { body: b }` / `return b`) are already handled by
    /// `suppress_source_vec_cleanup_for_arg`. Call AFTER `compile_expr`
    /// (which stages the acc) and BEFORE the scope-cleanup walk.
    /// Conservative side-effect-freedom check for f-string interpolation
    /// parts — the gate for the pre-sized single-malloc fast path in the
    /// `InterpolatedStringLit` arm. A part that passes can neither mutate
    /// nor free any buffer another part's rendered `(ptr, len)` aliases,
    /// so the deferred copies observe the same bytes the interpreter's
    /// (and the fallback path's) snapshot-per-part order does. Anything
    /// with call machinery (free fn, method, optional-chain, pipe) or
    /// control flow returns false and takes the append-per-part fallback
    /// — correct, just not pre-sized. `Index` can panic on OOB but cannot
    /// mutate; a mid-build panic aborts the process, so partial
    /// accumulator state is unobservable either way.
    fn fstr_part_is_side_effect_free(e: &Expr) -> bool {
        match &e.kind {
            ExprKind::Identifier(_)
            | ExprKind::SelfValue
            | ExprKind::Path { .. }
            | ExprKind::Integer(_, _)
            | ExprKind::Float(_, _)
            | ExprKind::Bool(_)
            | ExprKind::CharLit(_)
            | ExprKind::ByteLit(_)
            | ExprKind::StringLit(_)
            | ExprKind::MultiStringLit(_) => true,
            ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
                Self::fstr_part_is_side_effect_free(object)
            }
            ExprKind::Index { object, index } => {
                Self::fstr_part_is_side_effect_free(object)
                    && Self::fstr_part_is_side_effect_free(index)
            }
            ExprKind::Unary { operand, .. } => Self::fstr_part_is_side_effect_free(operand),
            ExprKind::Binary { left, right, .. } => {
                Self::fstr_part_is_side_effect_free(left)
                    && Self::fstr_part_is_side_effect_free(right)
            }
            ExprKind::Cast { expr, .. } => Self::fstr_part_is_side_effect_free(expr),
            _ => false,
        }
    }

    pub(super) fn suppress_fstr_acc_if_moved_out(&mut self, value: &Expr) {
        if matches!(value.kind, ExprKind::InterpolatedStringLit(_)) {
            if let Some(acc) = self.last_fstr_acc.take() {
                self.zero_vec_alloca_cap(acc);
            }
        }
    }

    /// Compile a `#[repr(C)] union Foo { ... }` literal — phase 5
    /// line 569 slice 4. See the dispatch comment at the top of
    /// `compile_struct_init` for the typechecker-supplied invariants
    /// codegen relies on (exactly-one field, valid field name, value
    /// type matches the field's declared type). The lowering shape is
    /// alloca → typed-store at the field's LLVM width → load the
    /// storage struct back. Padding bytes (when the union's max-size
    /// field is wider than its max-align field) stay undef — same
    /// observable contract Rust's union literals offer, and what the
    /// `unsafe { }` gate around any later field read holds the user
    /// responsible for.
    fn compile_union_init(
        &mut self,
        name: &str,
        storage_ty: inkwell::types::StructType<'ctx>,
        fields: &[FieldInit],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        // Defensive: an empty fields list shouldn't reach codegen
        // (slice 2c rejects empty union literals with a focused
        // diagnostic). Return undef of the storage type so we never
        // panic — keeps a hypothetical resolver/typecheck escape
        // contained to a well-formed-shape value.
        if fields.is_empty() {
            return Ok(storage_ty.get_undef().into());
        }
        // The typechecker rejects multi-field union literals — fields[0]
        // is the active variant. Re-validate the name against the
        // registered union members so a future codegen-only refactor
        // doesn't silently miscompile a slipped-through invariant.
        let field_init = &fields[0];
        let field_ll_ty: Option<BasicTypeEnum<'ctx>> = self
            .union_field_types
            .get(name)
            .and_then(|fs| fs.iter().find(|(n, _)| n == &field_init.name))
            .map(|(_, ty)| *ty);
        let val = self.compile_expr(&field_init.value)?;
        let slot = self
            .builder
            .build_alloca(storage_ty, &format!("union.{}.lit", name))
            .unwrap();
        // Untyped store of the value at the union's base address —
        // LLVM opaque pointers carry no pointee type, so this writes
        // exactly `val`'s natural width regardless of how `storage_ty`'s
        // first member is shaped. Reading the storage back via the
        // typed load below pulls those bytes (plus any uninitialized
        // padding) into an SSA aggregate the caller can move into its
        // own alloca.
        let _ = field_ll_ty; // reserved for future debug-info / typed-store ergonomics
        self.builder.build_store(slot, val).unwrap();
        let loaded = self
            .builder
            .build_load(storage_ty, slot, &format!("union.{}.val", name))
            .unwrap();
        Ok(loaded)
    }

    /// Compile `let <name>: Vec[T] = Vec::new()` for a SoA-laid-out collection.
    /// Produces `{ null, ..., [null_cold,] 0, 0 }` (one null ptr per group plus optional cold, len=0, cap=0).
    pub(super) fn compile_soa_new(
        &mut self,
        var_name: &str,
        soa: &SoaLayout,
    ) -> Result<(), String> {
        let fn_val = self.current_fn.unwrap();
        let has_cold = soa.cold_group.is_some();
        let soa_ty = self.soa_vec_type(soa.num_groups, has_cold);
        let null_ptr = self.context.ptr_type(AddressSpace::default()).const_null();
        let zero = self.context.i64_type().const_int(0, false);
        let len_idx = Self::soa_len_index(soa.num_groups, has_cold);
        let cap_idx = Self::soa_cap_index(soa.num_groups, has_cold);

        let mut agg = soa_ty.get_undef();
        // Hot group pointers.
        for i in 0..soa.num_groups {
            agg = self
                .builder
                .build_insert_value(agg, null_ptr, i as u32, &format!("soa.g{}", i))
                .unwrap()
                .into_struct_value();
        }
        // Cold pointer (if present).
        if has_cold {
            let cold_idx = Self::soa_cold_ptr_index(soa.num_groups);
            agg = self
                .builder
                .build_insert_value(agg, null_ptr, cold_idx, "soa.cold")
                .unwrap()
                .into_struct_value();
        }
        // len
        agg = self
            .builder
            .build_insert_value(agg, zero, len_idx, "soa.len")
            .unwrap()
            .into_struct_value();
        // cap
        agg = self
            .builder
            .build_insert_value(agg, zero, cap_idx, "soa.cap")
            .unwrap()
            .into_struct_value();

        let alloca = self.create_entry_alloca(fn_val, var_name, soa_ty.into());
        self.builder.build_store(alloca, agg).unwrap();
        self.variables.insert(
            var_name.to_string(),
            VarSlot {
                ptr: alloca,
                ty: soa_ty.into(),
            },
        );
        // Track for scope cleanup. SoA storage is multi-allocation (one
        // buffer per hot group + optional cold), so the cleanup routes
        // through `FreeSoaGroups` rather than `FreeVecBuffer` — the
        // latter would interpret the SoA alloca as `{ptr,len,cap}` and
        // both mis-read the cap slot and free only `g0`.
        let soa_drop_fn = self.emit_soa_drop_fn(soa);
        self.track_soa_groups(alloca, soa_ty, soa.num_groups as u32, has_cold, soa_drop_fn);
        Ok(())
    }

    /// Bind `let <var_name> = <call>()` where `var_name` is SoA and the callee
    /// returns a `Vec[E]` (backward inference, slice 3). Parks the receiving
    /// binding's layout in `pending_return_layout` so `compile_call` (which
    /// `take`s it) monomorphizes the callee to RETURN the SoA struct, then
    /// binds that struct into `var_name`'s SoA slot and tracks its group
    /// buffers for scope-exit cleanup — the caller now owns them (the callee
    /// suppressed its own `FreeSoaGroups` at the tail move-out). Mirrors
    /// `compile_soa_new`'s slot setup, storing the call result instead of a
    /// freshly-zeroed header. The let-arm gate
    /// (`let_rhs_calls_layout_returning_fn`) guarantees the dispatch fires, so
    /// `val` is the SoA struct the slot is typed for.
    pub(super) fn compile_soa_let_from_call(
        &mut self,
        var_name: &str,
        soa: &SoaLayout,
        value: &Expr,
    ) -> Result<(), String> {
        self.pending_return_layout = Some(self.active_layout_id(var_name));
        let val = self.compile_expr(value)?;
        // `compile_call` already `take`s the pending layout; clear defensively
        // so it can never leak into a later, unrelated call.
        self.pending_return_layout = None;

        let has_cold = soa.cold_group.is_some();
        let soa_ty = self.soa_vec_type(soa.num_groups, has_cold);
        let fn_val = self.current_fn.unwrap();
        let alloca = self.create_entry_alloca(fn_val, var_name, soa_ty.into());
        self.builder.build_store(alloca, val).unwrap();
        self.variables.insert(
            var_name.to_string(),
            VarSlot {
                ptr: alloca,
                ty: soa_ty.into(),
            },
        );
        let soa_drop_fn = self.emit_soa_drop_fn(soa);
        self.track_soa_groups(alloca, soa_ty, soa.num_groups as u32, has_cold, soa_drop_fn);
        Ok(())
    }

    /// Assignment sibling of `compile_soa_let_from_call`: `grid = f(grid, …)`
    /// where `grid` is an existing SoA binding and `f` returns a `Vec[E]` — the
    /// carried-grid double-buffer move at the heart of a stateful sim
    /// (`grid = substep(grid, s, workers)` each frame). Parks the binding's
    /// layout in `pending_return_layout` so the callee is return-SoA
    /// monomorphized (its result IS the 4-field struct), frees the OLD group
    /// buffers, then stores the new struct into the SAME slot.
    ///
    /// **Ownership.** A by-value SoA param is caller-retains (the callee borrows
    /// the moved-in header sharing the caller's group buffers — see
    /// `compile_mono_function`'s prologue), so after the call the OLD buffers are
    /// still live and owned here; the reassignment must free them or they leak
    /// every frame. The call is compiled FIRST (it reads the old `grid` as its
    /// argument), THEN the old groups are freed (read from the slot, which still
    /// holds the old header — the fresh return shares no buffer with it), THEN
    /// the new header overwrites the slot. The binding's queued `FreeSoaGroups`
    /// (registered at its `let`, keyed by this alloca) fires once at scope exit,
    /// reading whatever header the slot holds then — i.e. the final frame's
    /// buffers — so there is no double-free and no per-frame leak.
    pub(super) fn compile_soa_assign_from_call(
        &mut self,
        var_name: &str,
        soa: &SoaLayout,
        value: &Expr,
    ) -> Result<(), String> {
        let Some(slot) = self.variables.get(var_name).copied() else {
            // No existing slot (shouldn't happen for a live SoA binding); fall
            // back to the let-shape, which creates one.
            return self.compile_soa_let_from_call(var_name, soa, value);
        };
        self.pending_return_layout = Some(self.active_layout_id(var_name));
        let val = self.compile_expr(value)?;
        self.pending_return_layout = None;

        let has_cold = soa.cold_group.is_some();
        let soa_ty = self.soa_vec_type(soa.num_groups, has_cold);
        // Free the OLD group buffers (the header the slot currently holds),
        // mirroring the `FreeSoaGroups` cleanup walker: cap > 0 guards that
        // groups were actually allocated, then drop each live element's heap
        // (String/Vec) fields and free each hot (+ optional cold) buffer in
        // declaration order. Without the per-element drop, every frame's old
        // elements' String/Vec buffers leak on the carried-grid double-buffer
        // (`grid = substep(grid)`) — the per-frame analog of the scope leak.
        let soa_drop_fn = self.emit_soa_drop_fn(soa);
        self.emit_free_soa_groups_inline(
            slot.ptr,
            soa_ty,
            soa.num_groups as u32,
            has_cold,
            soa_drop_fn,
        );
        // Store the new header into the same slot; the binding's existing
        // queued `FreeSoaGroups` will free THESE buffers at scope exit.
        self.builder.build_store(slot.ptr, val).unwrap();
        Ok(())
    }

    /// Inline cap-guarded free of an SoA value's group buffers, reading the
    /// header from `soa_alloca`. Shares the shape of the `FreeSoaGroups` scope-
    /// cleanup arm (runtime.rs) but emitted eagerly — used by the SoA
    /// reassignment path to release the displaced (old) buffers.
    pub(super) fn emit_free_soa_groups_inline(
        &mut self,
        soa_alloca: PointerValue<'ctx>,
        soa_ty: inkwell::types::StructType<'ctx>,
        num_hot_groups: u32,
        has_cold: bool,
        soa_drop_fn: Option<FunctionValue<'ctx>>,
    ) {
        let i64_t = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let fn_val = self.current_fn.unwrap();
        let cap_idx = num_hot_groups + if has_cold { 1 } else { 0 } + 1;
        let cap_ptr = self
            .builder
            .build_struct_gep(soa_ty, soa_alloca, cap_idx, "soa.reassign.cap.ptr")
            .unwrap();
        let cap = self
            .builder
            .build_load(i64_t, cap_ptr, "soa.reassign.cap")
            .unwrap()
            .into_int_value();
        let zero = i64_t.const_int(0, false);
        let is_heap = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::UGT,
                cap,
                zero,
                "soa.reassign.is_heap",
            )
            .unwrap();
        let free_bb = self.context.append_basic_block(fn_val, "soa.reassign.free");
        let cont_bb = self.context.append_basic_block(fn_val, "soa.reassign.cont");
        self.builder
            .build_conditional_branch(is_heap, free_bb, cont_bb)
            .unwrap();

        self.builder.position_at_end(free_bb);
        // Drop each live element's heap (String/Vec) fields BEFORE the buffers
        // that hold them are freed. `None` for a POD layout (no IR — the
        // reassignment stays byte-identical). The loop is `[0, len)`, so a
        // `cap > 0`, `len == 0` header is a no-op.
        if let Some(drop_fn) = soa_drop_fn {
            self.builder
                .build_call(drop_fn, &[soa_alloca.into()], "")
                .unwrap();
        }
        let total_ptrs = num_hot_groups + if has_cold { 1 } else { 0 };
        for gi in 0..total_ptrs {
            let grp_ptr_ptr = self
                .builder
                .build_struct_gep(soa_ty, soa_alloca, gi, &format!("soa.reassign.g{}.ptr", gi))
                .unwrap();
            let grp_ptr = self
                .builder
                .build_load(ptr_ty, grp_ptr_ptr, &format!("soa.reassign.g{}.buf", gi))
                .unwrap()
                .into_pointer_value();
            self.builder
                .build_call(self.free_fn, &[grp_ptr.into()], "")
                .unwrap();
        }
        self.builder.build_unconditional_branch(cont_bb).unwrap();
        self.builder.position_at_end(cont_bb);
    }

    pub(super) fn compile_soa_method(
        &mut self,
        var_name: &str,
        soa: &SoaLayout,
        slot: VarSlot<'ctx>,
        method: &str,
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let has_cold = soa.cold_group.is_some();
        let soa_ty = self.soa_vec_type(soa.num_groups, has_cold);
        let i64_t = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let len_idx = Self::soa_len_index(soa.num_groups, has_cold);
        let cap_idx = Self::soa_cap_index(soa.num_groups, has_cold);

        // A `ref`/`mut ref Vec[E]` SoA param's slot holds a POINTER to the
        // caller's SoA struct; deref once so reads (`len`) and mutations
        // (`push`/`pop`/`remove` through `mut ref`) act on the caller's struct.
        // A by-value/`let` binding's slot already IS the struct. (Same boundary-
        // crossing deref as `compile_soa_index_read`.)
        let soa_struct_ptr = if self.ref_params.contains_key(var_name) {
            self.builder
                .build_load(ptr_ty, slot.ptr, "soa.ref.deref")
                .unwrap()
                .into_pointer_value()
        } else {
            slot.ptr
        };

        match method {
            "len" => {
                let len_ptr = self
                    .builder
                    .build_struct_gep(soa_ty, soa_struct_ptr, len_idx, "soa.len.ptr")
                    .unwrap();
                let len = self.builder.build_load(i64_t, len_ptr, "soa.len").unwrap();
                Ok(len)
            }
            "push" => {
                if args.is_empty() {
                    return Err("push requires an argument".to_string());
                }
                let elem_val = self.compile_expr(&args[0].value)?;
                let elem_sv = elem_val.into_struct_value();

                // Load len, cap.
                let len_ptr = self
                    .builder
                    .build_struct_gep(soa_ty, soa_struct_ptr, len_idx, "soa.len.ptr")
                    .unwrap();
                let cap_ptr = self
                    .builder
                    .build_struct_gep(soa_ty, soa_struct_ptr, cap_idx, "soa.cap.ptr")
                    .unwrap();
                let len = self
                    .builder
                    .build_load(i64_t, len_ptr, "soa.len")
                    .unwrap()
                    .into_int_value();
                let cap = self
                    .builder
                    .build_load(i64_t, cap_ptr, "soa.cap")
                    .unwrap()
                    .into_int_value();

                // Growth check.
                let fn_val = self.current_fn.unwrap();
                let grow_bb = self.context.append_basic_block(fn_val, "soa.grow");
                let store_bb = self.context.append_basic_block(fn_val, "soa.store");
                let needs_grow = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::EQ, len, cap, "soa.needs_grow")
                    .unwrap();
                self.builder
                    .build_conditional_branch(needs_grow, grow_bb, store_bb)
                    .unwrap();

                // Grow each group buffer.
                self.builder.position_at_end(grow_bb);
                let two = i64_t.const_int(2, false);
                let four = i64_t.const_int(4, false);
                let doubled = self.builder.build_int_mul(cap, two, "doubled").unwrap();
                let cmp = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::UGT, doubled, four, "cmp")
                    .unwrap();
                let new_cap = self
                    .builder
                    .build_select(cmp, doubled, four, "new_cap")
                    .unwrap()
                    .into_int_value();

                // Collect all groups to grow: hot groups first, then cold (if present).
                let cold_group_vec: Vec<(usize, &SoaGroup)> = if let Some(ref cg) = soa.cold_group {
                    let cold_idx = soa.num_groups; // struct field index for cold ptr
                    vec![(cold_idx, cg)]
                } else {
                    Vec::new()
                };
                let all_groups: Vec<(usize, &SoaGroup)> = soa
                    .groups
                    .iter()
                    .enumerate()
                    .chain(cold_group_vec.iter().copied())
                    .collect();

                for (struct_field_idx, group) in &all_groups {
                    let group_elem_ty = self.soa_group_elem_type(&soa.struct_name, group);
                    let elem_size = group_elem_ty.size_of().unwrap();
                    let alloc_bytes = self
                        .builder
                        .build_int_mul(new_cap, elem_size, "g.alloc")
                        .unwrap();
                    // Use aligned malloc for groups with align(N).
                    let new_buf = if let Some(align_n) = group.align {
                        let align_val = i64_t.const_int(align_n as u64, false);
                        self.builder
                            .build_call(
                                self.aligned_alloc_fn(),
                                &[align_val.into(), alloc_bytes.into()],
                                "g.new_aligned",
                            )
                            .unwrap()
                            .try_as_basic_value()
                            .unwrap_basic()
                            .into_pointer_value()
                    } else {
                        self.builder
                            .build_call(self.malloc_fn, &[alloc_bytes.into()], "g.new")
                            .unwrap()
                            .try_as_basic_value()
                            .unwrap_basic()
                            .into_pointer_value()
                    };
                    // Copy old data.
                    let old_ptr_ptr = self
                        .builder
                        .build_struct_gep(
                            soa_ty,
                            soa_struct_ptr,
                            *struct_field_idx as u32,
                            &format!("g{}.ptr", struct_field_idx),
                        )
                        .unwrap();
                    let old_buf = self
                        .builder
                        .build_load(ptr_ty, old_ptr_ptr, "g.old")
                        .unwrap()
                        .into_pointer_value();
                    let old_bytes = self
                        .builder
                        .build_int_mul(len, elem_size, "g.old_bytes")
                        .unwrap();
                    self.builder
                        .build_memcpy(new_buf, 8, old_buf, 8, old_bytes)
                        .unwrap();
                    self.builder
                        .build_call(self.free_fn, &[old_buf.into()], "")
                        .unwrap();
                    self.builder.build_store(old_ptr_ptr, new_buf).unwrap();
                }
                self.builder.build_store(cap_ptr, new_cap).unwrap();
                self.builder.build_unconditional_branch(store_bb).unwrap();

                // Store: decompose the struct into group fields.
                self.builder.position_at_end(store_bb);
                let cur_len = self
                    .builder
                    .build_load(i64_t, len_ptr, "cur_len")
                    .unwrap()
                    .into_int_value();

                // Store hot groups.
                for (gi, group) in soa.groups.iter().enumerate() {
                    let group_elem_ty = self.soa_group_elem_type(&soa.struct_name, group);
                    let grp_ptr_ptr = self
                        .builder
                        .build_struct_gep(
                            soa_ty,
                            soa_struct_ptr,
                            gi as u32,
                            &format!("g{}.ptr", gi),
                        )
                        .unwrap();
                    let grp_buf = self
                        .builder
                        .build_load(ptr_ty, grp_ptr_ptr, &format!("g{}.buf", gi))
                        .unwrap()
                        .into_pointer_value();
                    let dest = unsafe {
                        self.builder
                            .build_gep(group_elem_ty, grp_buf, &[cur_len], &format!("g{}.dest", gi))
                            .unwrap()
                    };
                    let mut grp_val = group_elem_ty.get_undef();
                    for (fi, &src_idx) in group.field_indices.iter().enumerate() {
                        let field_val = self
                            .builder
                            .build_extract_value(elem_sv, src_idx as u32, "f")
                            .unwrap();
                        grp_val = self
                            .builder
                            .build_insert_value(grp_val, field_val, fi as u32, "gf")
                            .unwrap()
                            .into_struct_value();
                    }
                    self.builder.build_store(dest, grp_val).unwrap();
                }
                // Store cold group (separate allocation).
                if let Some(ref cold) = soa.cold_group.clone() {
                    let cold_idx = Self::soa_cold_ptr_index(soa.num_groups);
                    let cold_elem_ty = self.soa_group_elem_type(&soa.struct_name, cold);
                    let cold_ptr_ptr = self
                        .builder
                        .build_struct_gep(soa_ty, soa_struct_ptr, cold_idx, "cold.ptr")
                        .unwrap();
                    let cold_buf = self
                        .builder
                        .build_load(ptr_ty, cold_ptr_ptr, "cold.buf")
                        .unwrap()
                        .into_pointer_value();
                    let dest = unsafe {
                        self.builder
                            .build_gep(cold_elem_ty, cold_buf, &[cur_len], "cold.dest")
                            .unwrap()
                    };
                    let mut cold_val = cold_elem_ty.get_undef();
                    for (fi, &src_idx) in cold.field_indices.iter().enumerate() {
                        let field_val = self
                            .builder
                            .build_extract_value(elem_sv, src_idx as u32, "f")
                            .unwrap();
                        cold_val = self
                            .builder
                            .build_insert_value(cold_val, field_val, fi as u32, "cf")
                            .unwrap()
                            .into_struct_value();
                    }
                    self.builder.build_store(dest, cold_val).unwrap();
                }

                // Increment len.
                let one = i64_t.const_int(1, false);
                let new_len = self.builder.build_int_add(cur_len, one, "new_len").unwrap();
                self.builder.build_store(len_ptr, new_len).unwrap();

                // Move-in of a NAMED owned struct binding
                // (`let c = Cell{..}; grid.push(c)`): the scatter above bit-
                // copied `c`'s fields — including any String/Vec buffer pointer
                // — into the group buffers, so the SoA Vec now owns them. Zero
                // the source binding's heap-field caps so its own `StructDrop`
                // no-ops on the `cap > 0` guard; otherwise both the source's
                // drop and the SoA cleanup free the same buffer (a double-free
                // ASAN catches). A struct-literal / temporary arg has no source
                // slot and is skipped; `zero_struct_move_caps` is itself a no-op
                // for a fully-POD element struct. The AoS-push peer of this is
                // `suppress_source_vec_cleanup_for_arg`.
                if let ExprKind::Identifier(src) = &args[0].value.kind {
                    if !self.ref_params.contains_key(src) {
                        if let Some(src_slot) = self.variables.get(src).copied() {
                            self.zero_struct_move_caps(src_slot.ptr, &soa.struct_name);
                        }
                    }
                }

                Ok(self.context.i64_type().const_int(0, false).into())
            }
            // `pop` / `pop_back` / `pop_front` return `Option[Entity]`;
            // `remove(i)` returns `Entity` directly. All three share the
            // materialize-then-shift pattern: scatter-read the element
            // at the removal index from every group buffer into an AoS
            // element struct (the inverse of push's decompose-and-
            // scatter), optionally memmove each group's tail left, then
            // decrement the shared `len`. The scatter-read is a pure bit-
            // copy that MOVES the element out: its String/Vec buffer
            // pointers transfer to the returned AoS struct (the caller's
            // binding drops them), and the now-decremented `len` excludes
            // the vacated slot from the per-element drop loop, so the SoA
            // cleanup frees each remaining buffer exactly once — no leak,
            // no double-free. (For a shift, the duplicate header left in the
            // old tail slot sits beyond `len` and is likewise never freed.)
            "pop" | "pop_back" | "pop_front" => {
                let is_front = method == "pop_front";
                let len_ptr = self
                    .builder
                    .build_struct_gep(soa_ty, soa_struct_ptr, len_idx, "soa.pop.len.ptr")
                    .unwrap();
                let len = self
                    .builder
                    .build_load(i64_t, len_ptr, "soa.pop.len")
                    .unwrap()
                    .into_int_value();

                let fn_val = self.current_fn.unwrap();
                let empty_bb = self
                    .context
                    .append_basic_block(fn_val, &format!("soa.{method}.empty"));
                let some_bb = self
                    .context
                    .append_basic_block(fn_val, &format!("soa.{method}.some"));
                let merge_bb = self
                    .context
                    .append_basic_block(fn_val, &format!("soa.{method}.merge"));

                let zero = i64_t.const_int(0, false);
                let one = i64_t.const_int(1, false);
                let is_empty = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::EQ, len, zero, "soa.pop.is_empty")
                    .unwrap();
                self.builder
                    .build_conditional_branch(is_empty, empty_bb, some_bb)
                    .unwrap();

                // Empty: no shift, no len decrement.
                self.builder.position_at_end(empty_bb);
                self.builder.build_unconditional_branch(merge_bb).unwrap();

                // Some: materialize at read_idx (0 for front, len-1 for back).
                self.builder.position_at_end(some_bb);
                let read_idx = if is_front {
                    zero
                } else {
                    self.builder
                        .build_int_sub(len, one, "soa.pop.last_idx")
                        .unwrap()
                };
                let elem_val = self.soa_materialize_at(soa, slot, soa_ty, read_idx);

                // pop_front: shift each group's [1..len] left by one
                // element. pop_back: no shift needed (the trailing
                // slot just falls out of `len`).
                if is_front {
                    let tail_count = self
                        .builder
                        .build_int_sub(len, one, "soa.pop_front.tail_count")
                        .unwrap();
                    self.soa_shift_groups_left(soa, slot, soa_ty, one, zero, tail_count);
                }

                let new_len = self
                    .builder
                    .build_int_sub(len, one, "soa.pop.new_len")
                    .unwrap();
                self.builder.build_store(len_ptr, new_len).unwrap();
                let some_payload_words = self.coerce_to_payload_words(elem_val.into(), 3)?;
                let some_end_bb = self.builder.get_insert_block().unwrap();
                self.builder.build_unconditional_branch(merge_bb).unwrap();

                // Merge: build Option struct via phi on tag + payload words.
                self.builder.position_at_end(merge_bb);
                let option_ty = self.enum_layouts["Option"].llvm_type;
                let tag_phi = self.builder.build_phi(i64_t, "soa.pop.opt.tag").unwrap();
                tag_phi.add_incoming(&[(&zero, empty_bb), (&one, some_end_bb)]);
                let mut word_phis: Vec<inkwell::values::PhiValue<'ctx>> =
                    Vec::with_capacity(some_payload_words.len());
                for (i, w) in some_payload_words.iter().enumerate() {
                    let word_phi = self
                        .builder
                        .build_phi(i64_t, &format!("soa.pop.opt.w{i}"))
                        .unwrap();
                    word_phi.add_incoming(&[(&zero, empty_bb), (w, some_end_bb)]);
                    word_phis.push(word_phi);
                }
                let mut agg: BasicValueEnum<'ctx> = option_ty.get_undef().into();
                agg = self
                    .builder
                    .build_insert_value(
                        agg.into_struct_value(),
                        tag_phi.as_basic_value(),
                        0,
                        "soa.pop.opt.tag.ins",
                    )
                    .unwrap()
                    .into_struct_value()
                    .into();
                for (i, phi) in word_phis.iter().enumerate() {
                    agg = self
                        .builder
                        .build_insert_value(
                            agg.into_struct_value(),
                            phi.as_basic_value(),
                            (i + 1) as u32,
                            &format!("soa.pop.opt.w{i}.ins"),
                        )
                        .unwrap()
                        .into_struct_value()
                        .into();
                }
                Ok(agg)
            }
            // `remove(idx) -> T` — materialize at `idx`, shift the tail
            // down in every group buffer, decrement len, return the
            // removed element. Mirrors plain `Vec.remove` (no Option
            // wrap, no bounds check — caller responsibility, matching
            // Rust's contract).
            "remove" => {
                if args.is_empty() {
                    return Err("SoA Vec.remove requires an index argument".to_string());
                }
                let idx_val = self.compile_expr(&args[0].value)?.into_int_value();
                let len_ptr = self
                    .builder
                    .build_struct_gep(soa_ty, soa_struct_ptr, len_idx, "soa.remove.len.ptr")
                    .unwrap();
                let len = self
                    .builder
                    .build_load(i64_t, len_ptr, "soa.remove.len")
                    .unwrap()
                    .into_int_value();
                let one = i64_t.const_int(1, false);

                let elem_val = self.soa_materialize_at(soa, slot, soa_ty, idx_val);

                // memmove(group[idx], group[idx+1], (len - 1 - idx) * sizeof(group_elem))
                let new_len = self
                    .builder
                    .build_int_sub(len, one, "soa.remove.new_len")
                    .unwrap();
                let tail_count = self
                    .builder
                    .build_int_sub(new_len, idx_val, "soa.remove.tail_count")
                    .unwrap();
                let next_idx = self
                    .builder
                    .build_int_add(idx_val, one, "soa.remove.next_idx")
                    .unwrap();
                self.soa_shift_groups_left(soa, slot, soa_ty, next_idx, idx_val, tail_count);

                self.builder.build_store(len_ptr, new_len).unwrap();
                Ok(elem_val.into())
            }
            // Catch-all so unsupported methods don't silently return 0
            // (the pre-2026-05-29 shape — masked many real codegen
            // gaps). New methods on SoA Vec must add a dedicated arm
            // above.
            other => Err(format!(
                "SoA Vec method '{other}' is not implemented; supported methods: \
                 len, push, pop, pop_back, pop_front, remove"
            )),
        }
    }

    /// Materialize the AoS element struct at `idx_val` in a SoA-laid-out
    /// Vec. Scatter-loads each group's sub-struct at `[idx_val]` and
    /// re-assembles fields into the original struct positions, the
    /// inverse of `compile_soa_method`'s push decomposition and the
    /// same shape used by `compile_soa_index_read`. Caller is
    /// responsible for bounds-checking `idx_val < len` — the helper
    /// emits no bounds check itself.
    fn soa_materialize_at(
        &mut self,
        soa: &SoaLayout,
        slot: VarSlot<'ctx>,
        soa_ty: inkwell::types::StructType<'ctx>,
        idx_val: inkwell::values::IntValue<'ctx>,
    ) -> inkwell::values::StructValue<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let elem_struct_ty = *self
            .struct_types
            .get(&soa.struct_name)
            .expect("SoA element struct missing in struct_types");
        let mut elem_val = elem_struct_ty.get_undef();
        let hot_groups = soa.groups.clone();
        for (gi, group) in hot_groups.iter().enumerate() {
            self.soa_scatter_group_into(
                &mut elem_val,
                soa,
                slot,
                soa_ty,
                ptr_ty,
                gi as u32,
                group,
                idx_val,
                &format!("g{}", gi),
            );
        }
        if let Some(cold) = soa.cold_group.clone() {
            let cold_idx = Self::soa_cold_ptr_index(soa.num_groups);
            self.soa_scatter_group_into(
                &mut elem_val,
                soa,
                slot,
                soa_ty,
                ptr_ty,
                cold_idx,
                &cold,
                idx_val,
                "cold",
            );
        }
        elem_val
    }

    /// Scatter-load one group's sub-struct at `[idx_val]` and insert
    /// each field back into the AoS element struct at its original
    /// position. Helper for `soa_materialize_at`.
    #[allow(clippy::too_many_arguments)]
    fn soa_scatter_group_into(
        &mut self,
        elem_val: &mut inkwell::values::StructValue<'ctx>,
        soa: &SoaLayout,
        slot: VarSlot<'ctx>,
        soa_ty: inkwell::types::StructType<'ctx>,
        ptr_ty: inkwell::types::PointerType<'ctx>,
        struct_field_idx: u32,
        group: &SoaGroup,
        idx_val: inkwell::values::IntValue<'ctx>,
        tag: &str,
    ) {
        let group_elem_ty = self.soa_group_elem_type(&soa.struct_name, group);
        let grp_ptr_ptr = self
            .builder
            .build_struct_gep(soa_ty, slot.ptr, struct_field_idx, &format!("{}.ptr", tag))
            .unwrap();
        let grp_buf = self
            .builder
            .build_load(ptr_ty, grp_ptr_ptr, &format!("{}.buf", tag))
            .unwrap()
            .into_pointer_value();
        let src = unsafe {
            self.builder
                .build_gep(group_elem_ty, grp_buf, &[idx_val], &format!("{}.src", tag))
                .unwrap()
        };
        let grp_val = self
            .builder
            .build_load(group_elem_ty, src, &format!("{}.val", tag))
            .unwrap()
            .into_struct_value();
        for (fi, &dst_idx) in group.field_indices.iter().enumerate() {
            let field_val = self
                .builder
                .build_extract_value(grp_val, fi as u32, "gf")
                .unwrap();
            *elem_val = self
                .builder
                .build_insert_value(*elem_val, field_val, dst_idx as u32, "ef")
                .unwrap()
                .into_struct_value();
        }
    }

    /// Shift each group's tail elements left by one element-slot:
    /// `memmove(group + dst_idx, group + src_idx, count * sizeof(group_elem))`.
    /// Used by `pop_front` (src_idx=1, dst_idx=0, count=len-1) and
    /// `remove` (src_idx=idx+1, dst_idx=idx, count=len-1-idx). Each
    /// group has its own element type and size, so the byte count is
    /// computed per group inside the helper.
    fn soa_shift_groups_left(
        &mut self,
        soa: &SoaLayout,
        slot: VarSlot<'ctx>,
        soa_ty: inkwell::types::StructType<'ctx>,
        src_idx: inkwell::values::IntValue<'ctx>,
        dst_idx: inkwell::values::IntValue<'ctx>,
        count: inkwell::values::IntValue<'ctx>,
    ) {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let hot_groups = soa.groups.clone();
        for (gi, group) in hot_groups.iter().enumerate() {
            self.soa_shift_one_group_left(
                soa,
                slot,
                soa_ty,
                ptr_ty,
                gi as u32,
                group,
                src_idx,
                dst_idx,
                count,
                &format!("g{}", gi),
            );
        }
        if let Some(cold) = soa.cold_group.clone() {
            let cold_idx = Self::soa_cold_ptr_index(soa.num_groups);
            self.soa_shift_one_group_left(
                soa, slot, soa_ty, ptr_ty, cold_idx, &cold, src_idx, dst_idx, count, "cold",
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn soa_shift_one_group_left(
        &mut self,
        soa: &SoaLayout,
        slot: VarSlot<'ctx>,
        soa_ty: inkwell::types::StructType<'ctx>,
        ptr_ty: inkwell::types::PointerType<'ctx>,
        struct_field_idx: u32,
        group: &SoaGroup,
        src_idx: inkwell::values::IntValue<'ctx>,
        dst_idx: inkwell::values::IntValue<'ctx>,
        count: inkwell::values::IntValue<'ctx>,
        tag: &str,
    ) {
        let group_elem_ty = self.soa_group_elem_type(&soa.struct_name, group);
        let elem_size = group_elem_ty.size_of().unwrap();
        let byte_count = self
            .builder
            .build_int_mul(count, elem_size, &format!("{}.shift.bytes", tag))
            .unwrap();
        let grp_ptr_ptr = self
            .builder
            .build_struct_gep(
                soa_ty,
                slot.ptr,
                struct_field_idx,
                &format!("{}.shift.ptr", tag),
            )
            .unwrap();
        let grp_buf = self
            .builder
            .build_load(ptr_ty, grp_ptr_ptr, &format!("{}.shift.buf", tag))
            .unwrap()
            .into_pointer_value();
        let src = unsafe {
            self.builder
                .build_gep(
                    group_elem_ty,
                    grp_buf,
                    &[src_idx],
                    &format!("{}.shift.src", tag),
                )
                .unwrap()
        };
        let dst = unsafe {
            self.builder
                .build_gep(
                    group_elem_ty,
                    grp_buf,
                    &[dst_idx],
                    &format!("{}.shift.dst", tag),
                )
                .unwrap()
        };
        self.builder
            .build_memmove(dst, 8, src, 8, byte_count)
            .unwrap();
    }
}
