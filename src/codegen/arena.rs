//! `Arena[T]` + `ArenaRef[T]` method codegen — `push` / `get` / `len` /
//! `high_water_mark` / `rewind_to`.
//!
//! `Arena[T]` lowers to the opaque `*mut KaracArena` handle returned by
//! `Arena.new()` (see `assoc_call.rs`); the blob table lives in
//! `runtime/src/arena.rs`. `ArenaRef[T]` erases to a bare `i64` index and
//! `ArenaCheckpoint` to a bare `i64` mark (`types_lowering.rs`) — the
//! baked two-field struct shapes exist for the interpreter's side-table
//! routing, which the AOT handle makes redundant.
//!
//! v1 element kinds (per monomorphized binding, recorded from the
//! `let a: Arena[T] = Arena.new()` annotation — the `Map.new()` precedent
//! already requires the annotation, since a later `push` can't infer `T`):
//! - `i64` / `f64` / `bool` — the 8-byte value blob; `get` copies it back
//!   out (`karac_runtime_arena_get_copy`, degrade = zeroes) and loads.
//! - `String` — byte content copied on push (the argument is only
//!   borrowed; a fresh-owned temp is materialized so its buffer frees at
//!   scope exit, exactly like `Interner.intern`); `get` hands back a
//!   borrowed `{ptr, len, cap = 0}` view of the stable arena-owned bytes.
//! - all-POD structs (every field `i64`/`f64`/`bool`) — the by-value byte
//!   image; `get` copies it into a fresh local and loads the struct value.
//!   This matches the interpreter, whose `get` clones the stored `Value`
//!   (writes through the returned `ref T` are not part of the v1 surface).
//!
//! Heap-owning element types (`Vec`, structs with `String` fields, shared
//! types, …) stay interpreter-only and fail loudly here.
//!
//! **Foreign-checkpoint guard.** The interpreter's `ArenaCheckpoint`
//! carries the minting arena's handle id; with the checkpoint erased to a
//! bare mark, codegen enforces the guard statically instead:
//! `arena_checkpoint_owner` records which arena binding minted each
//! checkpoint binding, and `rewind_to` with a foreign checkpoint compiles
//! to a no-op (the interpreter's "ignored" semantics).
//!
//! Receiver scope matches the `Interner` lowering: a LOCAL binding.
//! Passing an arena to another function stays interpreter-only — the
//! dispatch gate (`arena_vars`) only ever contains local bindings, so such
//! programs fail loudly at the user-impl fallthrough rather than
//! miscompile.

use crate::ast::*;

use inkwell::values::BasicValueEnum;

/// The element-type interpretation for one `Arena[T]` binding. Recorded at
/// the annotated `let` site; consulted by every `push` / `get` lowering.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ArenaElemKind {
    I64,
    F64,
    Bool,
    Str,
    /// All-POD struct (name into `struct_types`); POD-ness is validated at
    /// the first `push`/`get` so the error carries the method's span
    /// context rather than firing on an unused binding.
    Struct(String),
    /// Recognized `Arena[T]` annotation whose `T` this slice does not
    /// lower — kept so method calls fail with an actionable message
    /// instead of the generic user-impl fallthrough.
    Unsupported(String),
}

/// Classify the `T` of an `Arena[T]` annotation into an element kind.
/// Returns `None` when the annotation isn't a single-type-arg `Arena[…]`.
pub(super) fn classify_arena_annotation(te: &TypeExpr) -> Option<ArenaElemKind> {
    let TypeKind::Path(p) = &te.kind else {
        return None;
    };
    if p.segments.last().map(|s| s.as_str()) != Some("Arena") {
        return None;
    }
    let args = p.generic_args.as_ref()?;
    let [GenericArg::Type(elem)] = args.as_slice() else {
        return Some(ArenaElemKind::Unsupported(format!(
            "{} generic argument(s)",
            args.len()
        )));
    };
    let TypeKind::Path(ep) = &elem.kind else {
        return Some(ArenaElemKind::Unsupported("non-path element type".into()));
    };
    let name = ep.segments.last().map(|s| s.as_str()).unwrap_or("");
    Some(match name {
        "i64" => ArenaElemKind::I64,
        "f64" => ArenaElemKind::F64,
        "bool" => ArenaElemKind::Bool,
        "String" => ArenaElemKind::Str,
        _ if ep.generic_args.is_none() && ep.segments.len() == 1 => {
            ArenaElemKind::Struct(name.to_string())
        }
        other => ArenaElemKind::Unsupported(other.to_string()),
    })
}

/// `true` when the expression is a zero-arg `Arena.new()` associated call —
/// the local-binding initializer shape that queues the scope-exit
/// `FreeArenaHandle`. Mirrors `expr_is_interner_new`.
pub(super) fn expr_is_arena_new(expr: &Expr) -> bool {
    let ExprKind::Call { callee, args } = &expr.kind else {
        return false;
    };
    if !args.is_empty() {
        return false;
    }
    let ExprKind::Path { segments, .. } = &callee.kind else {
        return false;
    };
    segments.len() == 2 && segments[0] == "Arena" && segments[1] == "new"
}

impl<'ctx> super::Codegen<'ctx> {
    /// Lower an `Arena[T]` method call on a local binding `recv`. Dispatched
    /// from `compile_method_call` gated on `arena_vars` membership.
    pub(super) fn compile_arena_method(
        &mut self,
        recv: &str,
        method: &str,
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        match method {
            "push" => self.compile_arena_push(recv, args),
            "get" => self.compile_arena_get(recv, args),
            "len" | "high_water_mark" => self.compile_arena_len(recv),
            "rewind_to" => self.compile_arena_rewind_to(recv, args),
            _ => Err(format!(
                "codegen: unsupported Arena method `{method}` (only \
                 push/get/len/high_water_mark/rewind_to are lowered)"
            )),
        }
    }

    fn arena_elem_kind(&self, recv: &str) -> Result<ArenaElemKind, String> {
        match self.arena_vars.get(recv) {
            Some(ArenaElemKind::Unsupported(what)) => Err(format!(
                "codegen: Arena element type `{what}` is not lowered at v1 \
                 (supported: i64/f64/bool/String and all-POD structs) — \
                 `karac run --interp` executes it"
            )),
            Some(kind) => Ok(kind.clone()),
            None => Err(format!(
                "codegen: Arena binding '{recv}' has no recorded element \
                 type — annotate the binding (`let {recv}: Arena[T] = \
                 Arena.new()`)"
            )),
        }
    }

    /// Load the opaque `*mut KaracArena` handle from the binding's slot.
    fn load_arena_handle(
        &mut self,
        recv: &str,
    ) -> Result<inkwell::values::PointerValue<'ctx>, String> {
        let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let slot = self
            .get_data_ptr(recv)
            .ok_or_else(|| format!("unknown Arena binding '{recv}'"))?;
        Ok(self
            .builder
            .build_load(ptr_ty, slot, "arena.handle")
            .unwrap()
            .into_pointer_value())
    }

    /// The struct type + validated POD-ness for a `Struct(name)` element
    /// kind: every field must lower to `i64`/`f64`/`bool` so the by-value
    /// byte image owns no heap.
    fn arena_pod_struct_type(
        &self,
        name: &str,
    ) -> Result<inkwell::types::StructType<'ctx>, String> {
        let st = self.struct_types.get(name).copied().ok_or_else(|| {
            format!("codegen: Arena element struct `{name}` is not a known struct type")
        })?;
        let all_pod = self
            .struct_field_type_exprs
            .get(name)
            .map(|fields| {
                fields.iter().all(|f| {
                    matches!(
                        &f.kind,
                        TypeKind::Path(p) if matches!(
                            p.segments.last().map(|s| s.as_str()),
                            Some("i64") | Some("f64") | Some("bool")
                        )
                    )
                })
            })
            .unwrap_or(false);
        if !all_pod {
            return Err(format!(
                "codegen: Arena element struct `{name}` has non-POD fields \
                 (only i64/f64/bool fields are lowered at v1) — \
                 `karac run --interp` executes it"
            ));
        }
        Ok(st)
    }

    /// `arena.push(v) -> ArenaRef[T]` (a bare `i64` index). Marshals the
    /// value into a `(ptr, len)` blob the runtime copies.
    fn compile_arena_push(
        &mut self,
        recv: &str,
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let arg = args
            .first()
            .ok_or_else(|| "Arena.push expects a value argument".to_string())?;
        let kind = self.arena_elem_kind(recv)?;
        let handle = self.load_arena_handle(recv)?;
        let val = self.compile_expr(&arg.value)?;
        let i64_t = self.context.i64_type();
        let cur_fn = self.current_fn.unwrap();
        let (blob_ptr, blob_len) = match &kind {
            ArenaElemKind::I64 | ArenaElemKind::Bool => {
                if !val.is_int_value() {
                    return Err("codegen: Arena.push value/element type mismatch".to_string());
                }
                let as_i64 = self
                    .builder
                    .build_int_z_extend_or_bit_cast(val.into_int_value(), i64_t, "arena.push.v")
                    .unwrap();
                let slot = self.create_entry_alloca(cur_fn, "arena.push.slot", i64_t.into());
                self.builder.build_store(slot, as_i64).unwrap();
                (slot, i64_t.const_int(8, false))
            }
            ArenaElemKind::F64 => {
                if !val.is_float_value() {
                    return Err("codegen: Arena.push value/element type mismatch".to_string());
                }
                let f64_t = self.context.f64_type();
                let slot = self.create_entry_alloca(cur_fn, "arena.push.slot", f64_t.into());
                self.builder.build_store(slot, val).unwrap();
                (slot, i64_t.const_int(8, false))
            }
            ArenaElemKind::Str => {
                if !self.llvm_ty_is_vec_struct(val.get_type()) {
                    return Err("codegen: Arena.push value/element type mismatch".to_string());
                }
                let sstruct = val.into_struct_value();
                let data_ptr = self
                    .builder
                    .build_extract_value(sstruct, 0, "arena.push.sptr")
                    .unwrap()
                    .into_pointer_value();
                let len = self
                    .builder
                    .build_extract_value(sstruct, 1, "arena.push.slen")
                    .unwrap()
                    .into_int_value();
                let idx = self.call_arena_push(handle, data_ptr, len)?;
                // The runtime copied the bytes; a fresh-owned temp argument
                // (`push(a + b)`) would otherwise orphan its buffer — the
                // Interner.intern posture.
                let arg_key = (arg.value.span.offset, arg.value.span.length);
                if self.expr_yields_fresh_owned_temp(&arg.value)
                    && !self.rhs_stages_fstr_acc(&arg.value)
                {
                    self.materialize_owned_temp(val, arg_key);
                }
                return Ok(idx);
            }
            ArenaElemKind::Struct(name) => {
                let st = self.arena_pod_struct_type(name)?;
                if !val.is_struct_value() {
                    return Err("codegen: Arena.push value/element type mismatch".to_string());
                }
                let slot = self.create_entry_alloca(cur_fn, "arena.push.slot", st.into());
                self.builder.build_store(slot, val).unwrap();
                let size = st
                    .size_of()
                    .ok_or_else(|| format!("codegen: struct `{name}` has no computable size"))?;
                (slot, size)
            }
            ArenaElemKind::Unsupported(_) => unreachable!("filtered by arena_elem_kind"),
        };
        self.call_arena_push(handle, blob_ptr, blob_len)
    }

    fn call_arena_push(
        &mut self,
        handle: inkwell::values::PointerValue<'ctx>,
        ptr: inkwell::values::PointerValue<'ctx>,
        len: inkwell::values::IntValue<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let f = self
            .module
            .get_function("karac_runtime_arena_push")
            .expect("karac_runtime_arena_push declared in Codegen::new");
        Ok(self
            .builder
            .build_call(
                f,
                &[handle.into(), ptr.into(), len.into()],
                "arena.push.idx",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic())
    }

    /// `arena.get(r) -> ref T`. By-value kinds copy the blob back out
    /// (degrade = zeroes, matching the runtime's zero-fill); `String` hands
    /// back a borrowed `cap = 0` view of the stable arena-owned bytes.
    fn compile_arena_get(
        &mut self,
        recv: &str,
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let arg = args
            .first()
            .ok_or_else(|| "Arena.get expects an ArenaRef argument".to_string())?;
        let kind = self.arena_elem_kind(recv)?;
        let handle = self.load_arena_handle(recv)?;
        let idx = self.compile_expr(&arg.value)?;
        if !idx.is_int_value() {
            return Err("codegen: Arena.get argument must be an ArenaRef".to_string());
        }
        let idx = idx.into_int_value();
        let i64_t = self.context.i64_type();
        let cur_fn = self.current_fn.unwrap();
        match &kind {
            ArenaElemKind::I64 | ArenaElemKind::Bool | ArenaElemKind::F64 => {
                let slot_ty: inkwell::types::BasicTypeEnum = if kind == ArenaElemKind::F64 {
                    self.context.f64_type().into()
                } else {
                    i64_t.into()
                };
                let slot = self.create_entry_alloca(cur_fn, "arena.get.slot", slot_ty);
                self.call_arena_get_copy(handle, idx, slot, i64_t.const_int(8, false))?;
                let loaded = self
                    .builder
                    .build_load(slot_ty, slot, "arena.get.v")
                    .unwrap();
                if kind == ArenaElemKind::Bool {
                    let as_bool = self
                        .builder
                        .build_int_compare(
                            inkwell::IntPredicate::NE,
                            loaded.into_int_value(),
                            i64_t.const_zero(),
                            "arena.get.b",
                        )
                        .unwrap();
                    return Ok(as_bool.into());
                }
                Ok(loaded)
            }
            ArenaElemKind::Str => {
                let len_slot = self.create_entry_alloca(cur_fn, "arena.get.len.slot", i64_t.into());
                let f = self
                    .module
                    .get_function("karac_runtime_arena_get")
                    .expect("karac_runtime_arena_get declared in Codegen::new");
                let data_ptr = self
                    .builder
                    .build_call(
                        f,
                        &[handle.into(), idx.into(), len_slot.into()],
                        "arena.get.ptr",
                    )
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_pointer_value();
                let len = self
                    .builder
                    .build_load(i64_t, len_slot, "arena.get.len")
                    .unwrap()
                    .into_int_value();
                let str_ty = self.vec_struct_type();
                let with_ptr = self
                    .builder
                    .build_insert_value(str_ty.get_undef(), data_ptr, 0, "arena.get.s0")
                    .unwrap();
                let with_len = self
                    .builder
                    .build_insert_value(with_ptr, len, 1, "arena.get.s1")
                    .unwrap();
                let view = self
                    .builder
                    .build_insert_value(with_len, i64_t.const_zero(), 2, "arena.get.s2")
                    .unwrap()
                    .into_struct_value();
                Ok(view.into())
            }
            ArenaElemKind::Struct(name) => {
                let st = self.arena_pod_struct_type(name)?;
                let slot = self.create_entry_alloca(cur_fn, "arena.get.slot", st.into());
                let size = st
                    .size_of()
                    .ok_or_else(|| format!("codegen: struct `{name}` has no computable size"))?;
                self.call_arena_get_copy(handle, idx, slot, size)?;
                Ok(self.builder.build_load(st, slot, "arena.get.v").unwrap())
            }
            ArenaElemKind::Unsupported(_) => unreachable!("filtered by arena_elem_kind"),
        }
    }

    fn call_arena_get_copy(
        &mut self,
        handle: inkwell::values::PointerValue<'ctx>,
        idx: inkwell::values::IntValue<'ctx>,
        dst: inkwell::values::PointerValue<'ctx>,
        dst_len: inkwell::values::IntValue<'ctx>,
    ) -> Result<(), String> {
        let f = self
            .module
            .get_function("karac_runtime_arena_get_copy")
            .expect("karac_runtime_arena_get_copy declared in Codegen::new");
        self.builder
            .build_call(
                f,
                &[handle.into(), idx.into(), dst.into(), dst_len.into()],
                "arena.get.copied",
            )
            .unwrap();
        Ok(())
    }

    /// `arena.len()` / `arena.high_water_mark()` — the live item count (a
    /// checkpoint IS the current length, erased to a bare `i64` mark).
    fn compile_arena_len(&mut self, recv: &str) -> Result<BasicValueEnum<'ctx>, String> {
        let handle = self.load_arena_handle(recv)?;
        let f = self
            .module
            .get_function("karac_runtime_arena_len")
            .expect("karac_runtime_arena_len declared in Codegen::new");
        Ok(self
            .builder
            .build_call(f, &[handle.into()], "arena.len")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic())
    }

    /// `arena.rewind_to(cp)` — truncate back to the checkpoint's mark. The
    /// foreign-checkpoint guard is static: a checkpoint binding minted by a
    /// DIFFERENT arena compiles to a no-op (the interpreter's "ignored"
    /// semantics). Only a checkpoint held in a tracked local binding is
    /// lowered — anything else fails loudly rather than guess an owner.
    fn compile_arena_rewind_to(
        &mut self,
        recv: &str,
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let arg = args
            .first()
            .ok_or_else(|| "Arena.rewind_to expects an ArenaCheckpoint argument".to_string())?;
        // Unit return (the `i64 0` void placeholder).
        let unit: BasicValueEnum<'ctx> = self.context.i64_type().const_zero().into();
        let ExprKind::Identifier(cp_name) = &arg.value.kind else {
            return Err(
                "codegen: Arena.rewind_to argument must be a local checkpoint binding \
                 (`let cp = a.high_water_mark()`)"
                    .to_string(),
            );
        };
        let owner = self.arena_checkpoint_owner.get(cp_name.as_str()).cloned();
        match owner {
            Some(owner) if owner == recv => {
                let handle = self.load_arena_handle(recv)?;
                let mark = self.compile_expr(&arg.value)?;
                let f = self
                    .module
                    .get_function("karac_runtime_arena_rewind")
                    .expect("karac_runtime_arena_rewind declared in Codegen::new");
                self.builder
                    .build_call(
                        f,
                        &[handle.into(), mark.into_int_value().into()],
                        "arena.rewind",
                    )
                    .unwrap();
                Ok(unit)
            }
            // Foreign checkpoint: statically ignored, like the interpreter's
            // handle-id guard.
            Some(_) => Ok(unit),
            None => Err(format!(
                "codegen: `{cp_name}` is not a tracked ArenaCheckpoint binding \
                 (`let {cp_name} = <arena>.high_water_mark()`)"
            )),
        }
    }
}
