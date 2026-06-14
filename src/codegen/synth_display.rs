//! Display-fn synthesis: per-type `karac_display_<T>` LLVM functions.
//!
//! Houses the `emit_display_*` family that lazily synthesizes
//! display-rendering functions for every type the compiler can
//! `print` / `println` / interpolate. Per the design.md § Display
//! design, each function writes a textual representation to stdout
//! via `printf` without a trailing newline — callers append the `\n`
//! themselves.
//!
//! Cluster contents:
//!
//! - `emit_display_fn_for_type` — entry: primitive + compound dispatch
//! - `emit_vec_display_body` / `emit_vec_display_fn_te` — Vec[T] body
//! - `emit_map_display_fn` / `emit_map_display_body` — Map[K, V] body
//! - `emit_set_display_fn` / `emit_set_display_body` — Set[T] body
//! - `emit_tuple_display_fn` — tuple body
//! - `emit_display_fn_for_type_expr` — TypeExpr-keyed entry
//! - `display_mangle_te` — type-name mangler used for cache keys
//!
//! Lives in a sibling `impl<'ctx> super::Codegen<'ctx>` block.

use crate::ast::*;

use inkwell::module::Linkage;
use inkwell::types::{BasicType, BasicTypeEnum};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, FunctionValue, PointerValue};
use inkwell::AddressSpace;
use inkwell::IntPredicate;

impl<'ctx> super::Codegen<'ctx> {
    /// Emit (or reuse) a module-level Display function for the given type.
    ///
    /// Signature: `void karac_display_<type_name>(*const T)`. The function
    /// reads `*ptr` (or extracts struct fields, depending on the type) and
    /// writes a textual representation to stdout via `printf`. No trailing
    /// newline — callers append `\n` themselves for `println`.
    ///
    /// Subtask 1+2 scope: primitives (`i8`..`i64` / `u8`..`u64` / `f32`/`f64`
    /// / `bool` / `char` / `String`/`str`). Compound types (Vec/Map/Set/Tuple)
    /// land in subtasks 3-6, each as a new arm in this function that recurses
    /// into `emit_display_fn_for_type` for element/field types.
    ///
    /// Cache is keyed by the canonical `type_name` string — same convention
    /// used by `emit_hash_fn_for_type`. Caller is responsible for ensuring
    /// `type_name` uniquely identifies the type (for primitives this is
    /// trivial; for compound types the caller composes a mangled name).
    ///
    /// `dead_code` is allowed because subtasks 1+2 of the Display canonical
    /// bullet ship the machinery + primitive Display fns ahead of subtasks
    /// 3-7 which add the callers. Remove the allow when subtask 7 lands.
    /// Append a static string literal to the String accumulator `acc`. Used by
    /// the buffer-form Display fns. `self.current_fn` must be the Display fn
    /// being emitted so any buffer-grow blocks land in it (see
    /// `emit_string_append_raw`).
    pub(super) fn disp_append_lit(&mut self, acc: PointerValue<'ctx>, s: &str) {
        if s.is_empty() {
            return;
        }
        let g = self.builder.build_global_string_ptr(s, "dlit").unwrap();
        let len = self.context.i64_type().const_int(s.len() as u64, false);
        self.emit_string_append_raw(acc, g.as_pointer_value(), len);
    }

    /// Render a scalar via `snprintf` into a 64-byte stack buffer and append it
    /// to `acc`. `self.current_fn` must be the Display fn being emitted.
    pub(super) fn disp_append_snprintf(
        &mut self,
        acc: PointerValue<'ctx>,
        fmt: &str,
        arg: BasicMetadataValueEnum<'ctx>,
    ) {
        let i64_t = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let fn_val = self.current_fn.unwrap();
        let buf =
            self.create_entry_alloca(fn_val, "dbuf", self.context.i8_type().array_type(64).into());
        let buf_ptr = self
            .builder
            .build_pointer_cast(buf, ptr_ty, "dbufp")
            .unwrap();
        // snprintf's `size_t n` FIXED param is i32 on wasm32 / i64 natively;
        // match that width or the call mismatches the decl (B-2026-06-14-15).
        let size = if crate::target::active_target_is_wasm() {
            self.context.i32_type().const_int(64, false)
        } else {
            i64_t.const_int(64, false)
        };
        let fmt_g = self.builder.build_global_string_ptr(fmt, "dfmt").unwrap();
        let written = self
            .builder
            .build_call(
                self.snprintf_fn,
                &[
                    buf_ptr.into(),
                    size.into(),
                    fmt_g.as_pointer_value().into(),
                    arg,
                ],
                "dwr",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        let len = self
            .builder
            .build_int_z_extend(written, i64_t, "dwr64")
            .unwrap();
        self.emit_string_append_raw(acc, buf_ptr, len);
    }

    #[allow(dead_code)]
    pub(super) fn emit_display_fn_for_type(
        &mut self,
        type_name: &str,
        ty: BasicTypeEnum<'ctx>,
    ) -> FunctionValue<'ctx> {
        if let Some(&f) = self.display_fn_cache.get(type_name) {
            return f;
        }
        let fn_name = format!("karac_display_{type_name}");
        if let Some(f) = self.module.get_function(&fn_name) {
            self.display_fn_cache.insert(type_name.to_string(), f);
            return f;
        }

        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();

        let saved_bb = self.builder.get_insert_block();
        let saved_fn = self.current_fn;

        let display_fn_ty = self
            .context
            .void_type()
            .fn_type(&[ptr_ty.into(), ptr_ty.into()], false);
        let display_fn = self
            .module
            .add_function(&fn_name, display_fn_ty, Some(Linkage::Internal));
        self.display_fn_cache
            .insert(type_name.to_string(), display_fn);

        let entry_bb = self.context.append_basic_block(display_fn, "entry");
        self.builder.position_at_end(entry_bb);
        self.current_fn = Some(display_fn);
        let val_ptr = display_fn.get_nth_param(0).unwrap().into_pointer_value();
        let acc = display_fn.get_nth_param(1).unwrap().into_pointer_value();

        match type_name {
            "i8" | "i16" | "i32" | "i64" | "isize" => {
                let v = self
                    .builder
                    .build_load(ty, val_ptr, "v")
                    .unwrap()
                    .into_int_value();
                let v64 = self.builder.build_int_s_extend(v, i64_t, "v64").unwrap();
                self.disp_append_snprintf(acc, "%lld", v64.into());
            }
            "u8" | "u16" | "u32" | "u64" | "usize" => {
                let v = self
                    .builder
                    .build_load(ty, val_ptr, "v")
                    .unwrap()
                    .into_int_value();
                let v64 = self.builder.build_int_z_extend(v, i64_t, "v64").unwrap();
                self.disp_append_snprintf(acc, "%llu", v64.into());
            }
            "f32" | "f64" => {
                // Render with Rust's shortest-round-trip `{}` (via the runtime
                // formatter) so a struct's `Display` prints floats identically
                // to the interpreter — not C `%g`. `format_f64_to_stack_buf`
                // widens f32→f64 itself.
                let v = self
                    .builder
                    .build_load(ty, val_ptr, "v")
                    .unwrap()
                    .into_float_value();
                let (buf_ptr, len) = self.format_f64_to_stack_buf(v);
                self.emit_string_append_raw(acc, buf_ptr, len);
            }
            "bool" => {
                // Select "true"/"false" pointer AND length, then append.
                let v = self
                    .builder
                    .build_load(ty, val_ptr, "v")
                    .unwrap()
                    .into_int_value();
                let true_s = self.builder.build_global_string_ptr("true", "ts").unwrap();
                let false_s = self.builder.build_global_string_ptr("false", "fs").unwrap();
                let sel = self
                    .builder
                    .build_select(
                        v,
                        true_s.as_pointer_value(),
                        false_s.as_pointer_value(),
                        "bsel",
                    )
                    .unwrap()
                    .into_pointer_value();
                let four = i64_t.const_int(4, false);
                let five = i64_t.const_int(5, false);
                let len = self
                    .builder
                    .build_select(v, four, five, "blen")
                    .unwrap()
                    .into_int_value();
                self.emit_string_append_raw(acc, sel, len);
            }
            "char" => {
                // i32 codepoint → UTF-8 glyph bytes (better than the old
                // printf "%c" ASCII-only path).
                let v = self
                    .builder
                    .build_load(ty, val_ptr, "v")
                    .unwrap()
                    .into_int_value();
                let (p, l) = self.emit_codepoint_to_utf8(v);
                self.emit_string_append_raw(acc, p, l);
            }
            "String" | "str" => {
                // 24-byte struct {data, len, cap} — append the `len` bytes.
                let str_ty = self.vec_struct_type();
                let data_pp = self
                    .builder
                    .build_struct_gep(str_ty, val_ptr, 0, "s.data.pp")
                    .unwrap();
                let len_p = self
                    .builder
                    .build_struct_gep(str_ty, val_ptr, 1, "s.len.p")
                    .unwrap();
                let data = self
                    .builder
                    .build_load(ptr_ty, data_pp, "s.data")
                    .unwrap()
                    .into_pointer_value();
                let len = self
                    .builder
                    .build_load(i64_t, len_p, "s.len")
                    .unwrap()
                    .into_int_value();
                self.emit_string_append_raw(acc, data, len);
            }
            other if other.starts_with("Vec_") => {
                // Vec[T]'s element TypeExpr can't be unambiguously recovered
                // from the mangled cache name once nested compound shapes
                // (e.g. `Vec_tuple_i64_String`) are in play — string-splitting
                // on `_` is brittle. Callers should hold the element
                // `TypeExpr` and dispatch via `emit_display_fn_for_type_expr`,
                // which routes Vec to `emit_vec_display_fn_te(elem_te)`.
                panic!(
                    "emit_display_fn_for_type: '{other}' must be emitted via \
                     emit_vec_display_fn_te(elem_te) (or emit_display_fn_for_type_expr)"
                );
            }
            other if other.starts_with("Map_") => {
                // Map types have two type parameters and so cannot recover
                // (key_ty, val_ty) by string-splitting the cache key. Callers
                // that already hold K and V `TypeExpr`s should dispatch via
                // `emit_display_fn_for_type_expr`, which routes Map to
                // `emit_map_display_fn(key_te, val_te)`.
                panic!(
                    "emit_display_fn_for_type: '{other}' must be emitted via \
                     emit_map_display_fn(key_te, val_te) (or emit_display_fn_for_type_expr)"
                );
            }
            other if other.starts_with("Set_") => {
                // Set's element TypeExpr can't be unambiguously recovered
                // from a mangled cache name once nested compound shapes are
                // in play. Callers should hold the element `TypeExpr` and
                // dispatch via `emit_display_fn_for_type_expr`, which
                // routes Set to `emit_set_display_fn(elem_te)`.
                panic!(
                    "emit_display_fn_for_type: '{other}' must be emitted via \
                     emit_set_display_fn(elem_te) (or emit_display_fn_for_type_expr)"
                );
            }
            other if other.starts_with("tuple_") => {
                // n-tuples cannot recover their per-field TypeExprs from the
                // mangled name alone. Callers that already hold the field
                // `TypeExpr`s should dispatch via
                // `emit_display_fn_for_type_expr`, which routes Tuple to
                // `emit_tuple_display_fn(elems)`.
                panic!(
                    "emit_display_fn_for_type: '{other}' must be emitted via \
                     emit_tuple_display_fn(elems) (or emit_display_fn_for_type_expr)"
                );
            }
            other => {
                // User STRUCTS are rendered via `compile_struct_display_string`
                // (the synthetic-f-string path below), not this printf-based
                // synthesizer, so they never reach here. User ENUMS and any
                // remaining compound shapes are the open part of subtask 5 of
                // the Display canonical bullet (phase-8-stdlib-floor.md).
                panic!("emit_display_fn_for_type: type_name '{other}' not yet supported");
            }
        }

        self.builder.build_return(None).unwrap();

        self.current_fn = saved_fn;
        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }

        display_fn
    }

    /// Emit the body of a `Vec[T]` Display function. Reads `data`/`len` from
    /// the 24-byte Vec struct at `val_ptr`, prints `[`, walks elements with
    /// `, ` separators recursing into the element Display fn, prints `]`.
    ///
    /// `elem_te` describes the element type. Recursion into the per-element
    /// Display fn goes through the TypeExpr-aware dispatcher
    /// (`emit_display_fn_for_type_expr`) so compound elements (`Vec[Vec[T]]`,
    /// `Vec[(i64, String)]`, `Vec[Map[K, V]]`) compose correctly without the
    /// by-name path having to recover `TypeExpr`s from a mangled string.
    ///
    /// Caller is expected to have positioned the builder at the entry block
    /// of `display_fn` and to emit the trailing `ret void` after this returns.
    pub(super) fn emit_vec_display_body(
        &mut self,
        display_fn: FunctionValue<'ctx>,
        val_ptr: PointerValue<'ctx>,
        acc: PointerValue<'ctx>,
        elem_te: &TypeExpr,
    ) {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i8_t = self.context.i8_type();
        let i64_t = self.context.i64_type();
        let vec_ty = self.vec_struct_type();
        let elem_ty = self.llvm_type_for_type_expr(elem_te);

        // Materialize (or fetch) the element Display fn first — the recursive
        // emit may switch the builder's insert block, so do it before the
        // remaining body emission positions us at `display_fn`'s entry. The
        // dispatcher saves/restores so the caller's position is preserved.
        let elem_disp = self.emit_display_fn_for_type_expr(elem_te);

        self.disp_append_lit(acc, "[");

        // Load data (i8*) and len (i64) from the Vec struct.
        let data_pp = self
            .builder
            .build_struct_gep(vec_ty, val_ptr, 0, "v.data.pp")
            .unwrap();
        let len_p = self
            .builder
            .build_struct_gep(vec_ty, val_ptr, 1, "v.len.p")
            .unwrap();
        let data = self
            .builder
            .build_load(ptr_ty, data_pp, "v.data")
            .unwrap()
            .into_pointer_value();
        let len = self
            .builder
            .build_load(i64_t, len_p, "v.len")
            .unwrap()
            .into_int_value();

        // Element size in bytes — drives the GEP stride.
        let raw_size = elem_ty
            .size_of()
            .unwrap_or_else(|| i64_t.const_int(8, false));
        let elem_size = if raw_size.get_type().get_bit_width() == 64 {
            raw_size
        } else {
            self.builder
                .build_int_z_extend(raw_size, i64_t, "esz64")
                .unwrap()
        };

        // Loop: i in 0..len, with ", " separator before every elem after first.
        let pre_bb = self.builder.get_insert_block().unwrap();
        let hdr_bb = self.context.append_basic_block(display_fn, "vec.hdr");
        let bdy_bb = self.context.append_basic_block(display_fn, "vec.bdy");
        let sep_bb = self.context.append_basic_block(display_fn, "vec.sep");
        let elem_bb = self.context.append_basic_block(display_fn, "vec.elem");
        let exit_bb = self.context.append_basic_block(display_fn, "vec.exit");

        self.builder.build_unconditional_branch(hdr_bb).unwrap();

        self.builder.position_at_end(hdr_bb);
        let i_phi = self.builder.build_phi(i64_t, "vec.i").unwrap();
        i_phi.add_incoming(&[(&i64_t.const_zero(), pre_bb)]);
        let i_val = i_phi.as_basic_value().into_int_value();
        let cond = self
            .builder
            .build_int_compare(IntPredicate::ULT, i_val, len, "vec.cond")
            .unwrap();
        self.builder
            .build_conditional_branch(cond, bdy_bb, exit_bb)
            .unwrap();

        // bdy: branch to sep if i > 0, else straight to elem.
        self.builder.position_at_end(bdy_bb);
        let is_first = self
            .builder
            .build_int_compare(IntPredicate::EQ, i_val, i64_t.const_zero(), "is.first")
            .unwrap();
        self.builder
            .build_conditional_branch(is_first, elem_bb, sep_bb)
            .unwrap();

        // sep: append ", ", then fall to elem.
        self.builder.position_at_end(sep_bb);
        self.disp_append_lit(acc, ", ");
        self.builder.build_unconditional_branch(elem_bb).unwrap();

        // elem: GEP to data + i * elem_size, call element Display fn (acc).
        self.builder.position_at_end(elem_bb);
        let offset = self.builder.build_int_mul(i_val, elem_size, "off").unwrap();
        let elem_ptr = unsafe {
            self.builder
                .build_gep(i8_t, data, &[offset], "elem.p")
                .unwrap()
        };
        self.builder
            .build_call(elem_disp, &[elem_ptr.into(), acc.into()], "ed")
            .unwrap();
        let i_next = self
            .builder
            .build_int_add(i_val, i64_t.const_int(1, false), "vec.i1")
            .unwrap();
        // `i_next` may be produced in a continuation block if an append split
        // the elem block — read the current block for the phi incoming.
        let elem_end_bb = self.builder.get_insert_block().unwrap();
        i_phi.add_incoming(&[(&i_next, elem_end_bb)]);
        self.builder.build_unconditional_branch(hdr_bb).unwrap();

        // exit: append "]".
        self.builder.position_at_end(exit_bb);
        self.disp_append_lit(acc, "]");
    }

    /// Emit (or reuse) a Display function for `Map[K, V]`. Typed entry point —
    /// distinct from `emit_display_fn_for_type` because Map's two type
    /// parameters can't be recovered from a single mangled name string.
    ///
    /// The emitted function is named `karac_display_Map_<key>_<val>` (deeply
    /// mangled via `display_mangle_te`) and is shared with the generic Display
    /// cache under the same key, so a later `emit_display_fn_for_type` cache
    /// hit returns the same function (the catch-all `Map_*` arm panics on
    /// cache miss to steer callers here).
    ///
    /// Calling convention: `void karac_display_Map_K_V(ptr slot)` where `slot`
    /// is the address of a slot holding the opaque map handle (matches the
    /// shape produced by `compile_map_new_stmt`). Body loads the handle,
    /// drives `karac_map_iter_*` (mirroring `compile_for_map_var`),
    /// per-iteration recurses into `emit_display_fn_for_type_expr` for K and
    /// V (so `Map[(i64, String), Vec[bool]]` etc. compose correctly), and
    /// frees the iterator before returning. Iteration order is unspecified
    /// per `design.md` line 1588 — tests must not assert order.
    pub(super) fn emit_map_display_fn(
        &mut self,
        key_te: &TypeExpr,
        val_te: &TypeExpr,
    ) -> FunctionValue<'ctx> {
        let key_name = Self::display_mangle_te(key_te);
        let val_name = Self::display_mangle_te(val_te);
        let type_name = format!("Map_{key_name}_{val_name}");
        if let Some(&f) = self.display_fn_cache.get(&type_name) {
            return f;
        }
        let fn_name = format!("karac_display_{type_name}");
        if let Some(f) = self.module.get_function(&fn_name) {
            self.display_fn_cache.insert(type_name, f);
            return f;
        }

        let ptr_ty = self.context.ptr_type(AddressSpace::default());

        let saved_bb = self.builder.get_insert_block();
        let saved_fn = self.current_fn;

        let display_fn_ty = self
            .context
            .void_type()
            .fn_type(&[ptr_ty.into(), ptr_ty.into()], false);
        let display_fn = self
            .module
            .add_function(&fn_name, display_fn_ty, Some(Linkage::Internal));
        self.display_fn_cache.insert(type_name, display_fn);

        let entry_bb = self.context.append_basic_block(display_fn, "entry");
        self.builder.position_at_end(entry_bb);
        self.current_fn = Some(display_fn);
        let slot_ptr = display_fn.get_nth_param(0).unwrap().into_pointer_value();
        let acc = display_fn.get_nth_param(1).unwrap().into_pointer_value();

        self.emit_map_display_body(display_fn, slot_ptr, acc, key_te, val_te);

        self.builder.build_return(None).unwrap();

        self.current_fn = saved_fn;
        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }

        display_fn
    }

    /// Emit the body of a `Map[K, V]` Display function. Loads the map handle
    /// from `slot_ptr`, prints `"{"`, drives `karac_map_iter_new` /
    /// `karac_map_iter_next` to walk pairs, per-iteration recurses via
    /// `emit_display_fn_for_type_expr` for K and V with `": "` between
    /// key/value and `", "` between pairs, frees the iterator in the exit
    /// block, and prints `"}"`.
    ///
    /// `is_first` flag is tracked via an i1 alloca because the iterator-driven
    /// loop has no scalar counter (unlike Vec where `i == 0` works).
    ///
    /// Caller positions the builder at `display_fn`'s entry block and is
    /// responsible for emitting the trailing `ret void`.
    pub(super) fn emit_map_display_body(
        &mut self,
        display_fn: FunctionValue<'ctx>,
        slot_ptr: PointerValue<'ctx>,
        acc: PointerValue<'ctx>,
        key_te: &TypeExpr,
        val_te: &TypeExpr,
    ) {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let bool_t = self.context.bool_type();
        let key_ty = self.llvm_type_for_type_expr(key_te);
        let val_ty = self.llvm_type_for_type_expr(val_te);

        self.disp_append_lit(acc, "{");

        // Load the opaque map handle from slot_ptr.
        let map_handle = self
            .builder
            .build_load(ptr_ty, slot_ptr, "md.handle")
            .unwrap()
            .into_pointer_value();

        // Allocas for the loop's iterator handle, the is_first flag, and the
        // out_key / out_val staging slots. Place them in the entry block via
        // `create_entry_alloca` so they dominate the loop.
        let iter_slot = self.create_entry_alloca(display_fn, "md.iter.slot", ptr_ty.into());
        let first_slot = self.create_entry_alloca(display_fn, "md.first", bool_t.into());
        let out_key = self.create_entry_alloca(display_fn, "md.out_key", key_ty);
        let out_val = self.create_entry_alloca(display_fn, "md.out_val", val_ty);

        // Initialize iter, is_first.
        let iter_ptr = self
            .builder
            .build_call(self.karac_map_iter_new_fn, &[map_handle.into()], "md.iter")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        self.builder.build_store(iter_slot, iter_ptr).unwrap();
        self.builder
            .build_store(first_slot, bool_t.const_int(1, false))
            .unwrap();

        // Materialize (or fetch) the per-key and per-value Display fns.
        let key_disp = self.emit_display_fn_for_type_expr(key_te);
        let val_disp = self.emit_display_fn_for_type_expr(val_te);

        let hdr_bb = self.context.append_basic_block(display_fn, "map.hdr");
        let bdy_bb = self.context.append_basic_block(display_fn, "map.bdy");
        let sep_bb = self.context.append_basic_block(display_fn, "map.sep");
        let pair_bb = self.context.append_basic_block(display_fn, "map.pair");
        let exit_bb = self.context.append_basic_block(display_fn, "map.exit");

        self.builder.build_unconditional_branch(hdr_bb).unwrap();

        // hdr: advance iterator; loop while it returns true.
        self.builder.position_at_end(hdr_bb);
        let iter_cur = self
            .builder
            .build_load(ptr_ty, iter_slot, "md.iter.cur")
            .unwrap()
            .into_pointer_value();
        let has_next = self
            .builder
            .build_call(
                self.karac_map_iter_next_fn,
                &[iter_cur.into(), out_key.into(), out_val.into()],
                "md.next",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        self.builder
            .build_conditional_branch(has_next, bdy_bb, exit_bb)
            .unwrap();

        // bdy: branch on is_first — first iteration skips the ", " separator
        // and clears the flag; subsequent iterations print ", " first.
        self.builder.position_at_end(bdy_bb);
        let f = self
            .builder
            .build_load(bool_t, first_slot, "md.f")
            .unwrap()
            .into_int_value();
        self.builder
            .build_conditional_branch(f, pair_bb, sep_bb)
            .unwrap();

        // sep: append ", " then fall through to pair.
        self.builder.position_at_end(sep_bb);
        self.disp_append_lit(acc, ", ");
        self.builder.build_unconditional_branch(pair_bb).unwrap();

        // pair: clear is_first (idempotent on second+ iters), append key, ": ",
        // value, then loop back to hdr.
        self.builder.position_at_end(pair_bb);
        self.builder
            .build_store(first_slot, bool_t.const_int(0, false))
            .unwrap();
        self.builder
            .build_call(key_disp, &[out_key.into(), acc.into()], "md.kd")
            .unwrap();
        self.disp_append_lit(acc, ": ");
        self.builder
            .build_call(val_disp, &[out_val.into(), acc.into()], "md.vd")
            .unwrap();
        self.builder.build_unconditional_branch(hdr_bb).unwrap();

        // exit: free iterator, append "}".
        self.builder.position_at_end(exit_bb);
        let iter_final = self
            .builder
            .build_load(ptr_ty, iter_slot, "md.iter.final")
            .unwrap()
            .into_pointer_value();
        self.builder
            .build_call(self.karac_map_iter_free_fn, &[iter_final.into()], "")
            .unwrap();
        self.disp_append_lit(acc, "}");
    }

    /// Emit (or reuse) a Display function for `Set[T]`. Typed entry point —
    /// shape mirrors `emit_map_display_fn` minus the value-side Display
    /// (Set lowers to `Map[T, ()]`; the iterator's value out-slot is sized
    /// 0 and the contents are discarded).
    ///
    /// The emitted function is named `karac_display_Set_<elem>` (deeply
    /// mangled via `display_mangle_te`) and shares the generic Display
    /// cache. Format `Set{a, b, c}` with the literal `Set` prefix matches
    /// the interpreter at `src/interpreter.rs:292`. Iteration order is
    /// unspecified per `design.md` line 1588 — tests must not assert order.
    pub(super) fn emit_set_display_fn(&mut self, elem_te: &TypeExpr) -> FunctionValue<'ctx> {
        let elem_name = Self::display_mangle_te(elem_te);
        let type_name = format!("Set_{elem_name}");
        if let Some(&f) = self.display_fn_cache.get(&type_name) {
            return f;
        }
        let fn_name = format!("karac_display_{type_name}");
        if let Some(f) = self.module.get_function(&fn_name) {
            self.display_fn_cache.insert(type_name, f);
            return f;
        }

        let ptr_ty = self.context.ptr_type(AddressSpace::default());

        let saved_bb = self.builder.get_insert_block();
        let saved_fn = self.current_fn;

        let display_fn_ty = self
            .context
            .void_type()
            .fn_type(&[ptr_ty.into(), ptr_ty.into()], false);
        let display_fn = self
            .module
            .add_function(&fn_name, display_fn_ty, Some(Linkage::Internal));
        self.display_fn_cache.insert(type_name, display_fn);

        let entry_bb = self.context.append_basic_block(display_fn, "entry");
        self.builder.position_at_end(entry_bb);
        self.current_fn = Some(display_fn);
        let slot_ptr = display_fn.get_nth_param(0).unwrap().into_pointer_value();
        let acc = display_fn.get_nth_param(1).unwrap().into_pointer_value();

        self.emit_set_display_body(display_fn, slot_ptr, acc, elem_te);

        self.builder.build_return(None).unwrap();

        self.current_fn = saved_fn;
        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }

        display_fn
    }

    /// Body of the Set Display fn. Loads the opaque map handle (Set lowers
    /// to `Map[T, ()]`), prints `Set{`, walks `karac_map_iter_*` printing
    /// each element via the per-type Display fn with `, ` between, frees
    /// the iterator, prints `}`. The val out-slot is sized 0 — a single
    /// shared `i8` alloca — and its contents are discarded.
    pub(super) fn emit_set_display_body(
        &mut self,
        display_fn: FunctionValue<'ctx>,
        slot_ptr: PointerValue<'ctx>,
        acc: PointerValue<'ctx>,
        elem_te: &TypeExpr,
    ) {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let bool_t = self.context.bool_type();
        let i8_t = self.context.i8_type();
        let elem_ty = self.llvm_type_for_type_expr(elem_te);

        // "Set{" — literal prefix matches the interpreter format at
        // `src/interpreter.rs:292`.
        self.disp_append_lit(acc, "Set{");

        // Load the opaque set/map handle from slot_ptr.
        let set_handle = self
            .builder
            .build_load(ptr_ty, slot_ptr, "sd.handle")
            .unwrap()
            .into_pointer_value();

        let iter_slot = self.create_entry_alloca(display_fn, "sd.iter.slot", ptr_ty.into());
        let first_slot = self.create_entry_alloca(display_fn, "sd.first", bool_t.into());
        let out_elem = self.create_entry_alloca(display_fn, "sd.out_elem", elem_ty);
        // val_size = 0 — a single shared i8 alloca for the discarded
        // value out-slot. Runtime stores zero bytes regardless.
        let dummy_val = self.create_entry_alloca(display_fn, "sd.dummy", i8_t.into());

        let iter_ptr = self
            .builder
            .build_call(self.karac_map_iter_new_fn, &[set_handle.into()], "sd.iter")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        self.builder.build_store(iter_slot, iter_ptr).unwrap();
        self.builder
            .build_store(first_slot, bool_t.const_int(1, false))
            .unwrap();

        let elem_disp = self.emit_display_fn_for_type_expr(elem_te);

        let hdr_bb = self.context.append_basic_block(display_fn, "set.hdr");
        let bdy_bb = self.context.append_basic_block(display_fn, "set.bdy");
        let sep_bb = self.context.append_basic_block(display_fn, "set.sep");
        let elem_bb = self.context.append_basic_block(display_fn, "set.elem");
        let exit_bb = self.context.append_basic_block(display_fn, "set.exit");

        self.builder.build_unconditional_branch(hdr_bb).unwrap();

        self.builder.position_at_end(hdr_bb);
        let iter_cur = self
            .builder
            .build_load(ptr_ty, iter_slot, "sd.iter.cur")
            .unwrap()
            .into_pointer_value();
        let has_next = self
            .builder
            .build_call(
                self.karac_map_iter_next_fn,
                &[iter_cur.into(), out_elem.into(), dummy_val.into()],
                "sd.next",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        self.builder
            .build_conditional_branch(has_next, bdy_bb, exit_bb)
            .unwrap();

        self.builder.position_at_end(bdy_bb);
        let f = self
            .builder
            .build_load(bool_t, first_slot, "sd.f")
            .unwrap()
            .into_int_value();
        self.builder
            .build_conditional_branch(f, elem_bb, sep_bb)
            .unwrap();

        self.builder.position_at_end(sep_bb);
        self.disp_append_lit(acc, ", ");
        self.builder.build_unconditional_branch(elem_bb).unwrap();

        self.builder.position_at_end(elem_bb);
        self.builder
            .build_store(first_slot, bool_t.const_int(0, false))
            .unwrap();
        self.builder
            .build_call(elem_disp, &[out_elem.into(), acc.into()], "sd.ed")
            .unwrap();
        self.builder.build_unconditional_branch(hdr_bb).unwrap();

        self.builder.position_at_end(exit_bb);
        let iter_final = self
            .builder
            .build_load(ptr_ty, iter_slot, "sd.iter.final")
            .unwrap()
            .into_pointer_value();
        self.builder
            .build_call(self.karac_map_iter_free_fn, &[iter_final.into()], "")
            .unwrap();
        self.disp_append_lit(acc, "}");
    }

    /// Deeply mangled type name suitable for Display cache keys. Unlike
    /// `mangled_type_name` (which is shallow on `Path` types — used for
    /// hash/eq, where `Map[Vec[T], V]` is unreachable so deep mangling is
    /// unnecessary), this walks generic args so `Vec[i64]` → `Vec_i64`,
    /// `Map[String, i64]` → `Map_String_i64`, and nested shapes compose.
    /// Tuples use the same `tuple_T1_T2_...` form `mangled_type_name`
    /// produces — the recursive shapes match.
    pub(super) fn display_mangle_te(te: &TypeExpr) -> String {
        match &te.kind {
            TypeKind::Tuple(elems) if elems.is_empty() => "unit".to_string(),
            TypeKind::Tuple(elems) => {
                let parts: Vec<String> = elems.iter().map(Self::display_mangle_te).collect();
                format!("tuple_{}", parts.join("_"))
            }
            TypeKind::Path(p) => {
                let head = p
                    .segments
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "unknown".to_string());
                if let Some(args) = p.generic_args.as_ref() {
                    let parts: Vec<String> = args
                        .iter()
                        .filter_map(|a| match a {
                            GenericArg::Type(t) => Some(Self::display_mangle_te(t)),
                            _ => None,
                        })
                        .collect();
                    if !parts.is_empty() {
                        return format!("{head}_{}", parts.join("_"));
                    }
                }
                head
            }
            _ => "unknown".to_string(),
        }
    }

    /// TypeExpr-aware Display dispatcher. Canonical entry point for any
    /// caller that holds a source-level `TypeExpr`: routes by shape to the
    /// typed Vec / Map / Tuple entry points, and falls through to the
    /// by-name `emit_display_fn_for_type` for primitives. Mirror of
    /// `emit_hash_fn_for_type_expr` / `emit_eq_fn_for_type_expr`.
    ///
    /// Cache-key check up front so the dispatcher itself is cheap on repeat
    /// calls — every typed entry point (`emit_*_display_fn_te` /
    /// `emit_tuple_display_fn`) also re-checks before emitting, but doing it
    /// here avoids the per-shape branching cost when the function already
    /// exists.
    pub(super) fn emit_display_fn_for_type_expr(&mut self, te: &TypeExpr) -> FunctionValue<'ctx> {
        let type_name = Self::display_mangle_te(te);
        if let Some(&f) = self.display_fn_cache.get(&type_name) {
            return f;
        }
        let fn_name = format!("karac_display_{type_name}");
        if let Some(f) = self.module.get_function(&fn_name) {
            self.display_fn_cache.insert(type_name, f);
            return f;
        }

        match &te.kind {
            TypeKind::Tuple(elems) if !elems.is_empty() => self.emit_tuple_display_fn(elems),
            TypeKind::Path(p) => {
                let head = p.segments.first().map(String::as_str);
                if head == Some("Vec") {
                    if let Some(GenericArg::Type(elem_te)) =
                        p.generic_args.as_ref().and_then(|a| a.first()).cloned()
                    {
                        return self.emit_vec_display_fn_te(&elem_te);
                    }
                }
                if head == Some("Map") {
                    let args = p.generic_args.as_ref();
                    let k_te = args.and_then(|a| a.first()).and_then(|a| match a {
                        GenericArg::Type(t) => Some(t.clone()),
                        _ => None,
                    });
                    let v_te = args.and_then(|a| a.get(1)).and_then(|a| match a {
                        GenericArg::Type(t) => Some(t.clone()),
                        _ => None,
                    });
                    if let (Some(k), Some(v)) = (k_te, v_te) {
                        return self.emit_map_display_fn(&k, &v);
                    }
                }
                if head == Some("Set") {
                    if let Some(GenericArg::Type(elem_te)) =
                        p.generic_args.as_ref().and_then(|a| a.first()).cloned()
                    {
                        return self.emit_set_display_fn(&elem_te);
                    }
                }
                // User enum (possibly payload-bearing) — value-driven Display
                // fn (the all-unit `compile_unit_enum_display` is select-chain
                // and expr-driven; this path is the buffer-append, by-pointer
                // form needed for nested/recursive field rendering).
                if let Some(seg) = p.segments.last() {
                    if self.enum_layouts.contains_key(seg) {
                        return self.emit_enum_display_fn(seg);
                    }
                }
                // Primitive (or unsupported path) — fall through to by-name.
                let llvm_ty = self.llvm_type_for_type_expr(te);
                self.emit_display_fn_for_type(&type_name, llvm_ty)
            }
            _ => {
                let llvm_ty = self.llvm_type_for_type_expr(te);
                self.emit_display_fn_for_type(&type_name, llvm_ty)
            }
        }
    }

    /// Emit (or reuse) a typed Display function for `Vec[T]`. The function
    /// is named `karac_display_Vec_<elem_mangled>` and shares the generic
    /// `display_fn_cache` keyed on the same mangled name; the catch-all
    /// `Vec_*` arm in `emit_display_fn_for_type` panics on cache miss to
    /// steer callers here. Body delegates to `emit_vec_display_body` which
    /// recurses via `emit_display_fn_for_type_expr` for the element type.
    pub(super) fn emit_vec_display_fn_te(&mut self, elem_te: &TypeExpr) -> FunctionValue<'ctx> {
        let elem_name = Self::display_mangle_te(elem_te);
        let type_name = format!("Vec_{elem_name}");
        if let Some(&f) = self.display_fn_cache.get(&type_name) {
            return f;
        }
        let fn_name = format!("karac_display_{type_name}");
        if let Some(f) = self.module.get_function(&fn_name) {
            self.display_fn_cache.insert(type_name, f);
            return f;
        }

        let ptr_ty = self.context.ptr_type(AddressSpace::default());

        let saved_bb = self.builder.get_insert_block();
        let saved_fn = self.current_fn;

        let display_fn_ty = self
            .context
            .void_type()
            .fn_type(&[ptr_ty.into(), ptr_ty.into()], false);
        let display_fn = self
            .module
            .add_function(&fn_name, display_fn_ty, Some(Linkage::Internal));
        self.display_fn_cache.insert(type_name, display_fn);

        let entry_bb = self.context.append_basic_block(display_fn, "entry");
        self.builder.position_at_end(entry_bb);
        self.current_fn = Some(display_fn);
        let val_ptr = display_fn.get_nth_param(0).unwrap().into_pointer_value();
        let acc = display_fn.get_nth_param(1).unwrap().into_pointer_value();

        self.emit_vec_display_body(display_fn, val_ptr, acc, elem_te);

        self.builder.build_return(None).unwrap();

        self.current_fn = saved_fn;
        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }

        display_fn
    }

    /// Emit (or reuse) a typed Display function for an n-tuple
    /// `(T1, T2, …, Tn)`. Typed entry point — distinct from the by-name
    /// `emit_display_fn_for_type` because per-field `TypeExpr`s can't be
    /// recovered from a single mangled name string once nested compound
    /// shapes (`((i64, i64), String)`) are in play. Mirror of the
    /// `emit_map_display_fn` pattern.
    ///
    /// Cache key (and function name suffix) is the deeply-mangled name —
    /// `tuple_T1_T2_..._Tn`. Shares the generic `display_fn_cache` so a
    /// later `emit_display_fn_for_type` cache hit on the same name returns
    /// this function (the catch-all `tuple_*` arm panics on cache miss to
    /// steer callers here).
    ///
    /// Calling convention: `void karac_display_tuple_T1_T2_..._Tn(ptr p)`
    /// where `p` points to the LLVM tuple struct value (one alloca'd or
    /// in-struct field address). Body reads each field via `getelementptr`
    /// on the tuple's LLVM struct type, recurses via
    /// `emit_display_fn_for_type_expr` for each field, and prints
    /// `(field0, field1, ...)` with `, ` between fields. Format matches
    /// the interpreter's tuple Display at `src/interpreter.rs:215`.
    pub(super) fn emit_tuple_display_fn(&mut self, elems: &[TypeExpr]) -> FunctionValue<'ctx> {
        // Cache lookup. Compute the canonical name first so module + cache
        // checks share one key.
        let parts: Vec<String> = elems.iter().map(Self::display_mangle_te).collect();
        let type_name = format!("tuple_{}", parts.join("_"));
        let fn_name = format!("karac_display_{type_name}");
        if let Some(&f) = self.display_fn_cache.get(&type_name) {
            return f;
        }
        if let Some(f) = self.module.get_function(&fn_name) {
            self.display_fn_cache.insert(type_name, f);
            return f;
        }

        let elems_owned: Vec<TypeExpr> = elems.to_vec();

        // Materialize per-field Display fns first. Each recursive emit
        // saves and restores the builder position, so calling them before
        // we open this function's body is safe — the alternative (calling
        // mid-emission) would require careful position management.
        let field_disps: Vec<FunctionValue<'ctx>> = elems_owned
            .iter()
            .map(|e| self.emit_display_fn_for_type_expr(e))
            .collect();

        // Compute the tuple's LLVM struct type. Must match exactly what
        // `llvm_type_for_type_expr(Tuple(...))` produces so callers can pass
        // their tuple value's address directly to this function.
        let field_tys: Vec<BasicTypeEnum<'ctx>> = elems_owned
            .iter()
            .map(|e| self.llvm_type_for_type_expr(e))
            .collect();
        let tuple_ty = self.context.struct_type(&field_tys, false);
        let ptr_ty = self.context.ptr_type(AddressSpace::default());

        let saved_bb = self.builder.get_insert_block();
        let saved_fn = self.current_fn;
        let display_fn_ty = self
            .context
            .void_type()
            .fn_type(&[ptr_ty.into(), ptr_ty.into()], false);
        let display_fn = self
            .module
            .add_function(&fn_name, display_fn_ty, Some(Linkage::Internal));
        self.display_fn_cache.insert(type_name, display_fn);

        let entry_bb = self.context.append_basic_block(display_fn, "entry");
        self.builder.position_at_end(entry_bb);
        self.current_fn = Some(display_fn);
        let val_ptr = display_fn.get_nth_param(0).unwrap().into_pointer_value();
        let acc = display_fn.get_nth_param(1).unwrap().into_pointer_value();

        self.disp_append_lit(acc, "(");

        for (i, fd) in field_disps.iter().enumerate() {
            if i > 0 {
                self.disp_append_lit(acc, ", ");
            }
            let field_ptr = self
                .builder
                .build_struct_gep(tuple_ty, val_ptr, i as u32, &format!("t.f{i}.p"))
                .unwrap();
            self.builder
                .build_call(*fd, &[field_ptr.into(), acc.into()], &format!("t.f{i}.d"))
                .unwrap();
        }

        self.disp_append_lit(acc, ")");

        self.builder.build_return(None).unwrap();

        self.current_fn = saved_fn;
        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        display_fn
    }

    // ── User-struct Display (subtask 5) ────────────────────────────────
    //
    // `#[derive(Display)]` / `impl Display` structs render as
    // `TypeName { field: value, … }` in DECLARATION order, matching the
    // interpreter's `display_render`. Rather than synthesize a bespoke
    // recursive printf/buffer Display fn, we lower a struct render to the
    // equivalent **f-string AST** and reuse the existing interpolation
    // codegen (which already owns primitive / String formatting, buffer
    // growth, and scope-exit cleanup). Nested Display-struct fields are
    // inlined recursively so the synthetic f-string never carries a
    // struct-typed interpolation part (those would be mis-rendered as
    // String). Fields of other compound types (Vec / Map / Set / enum /
    // tuple) are not yet supported here and surface a clean codegen error.

    /// If `te` is a path to a user struct we know how to render, return its
    /// name. Used to decide recursion vs. leaf-interpolation per field.
    fn display_field_struct_name(&self, te: &crate::ast::TypeExpr) -> Option<String> {
        if let crate::ast::TypeKind::Path(p) = &te.kind {
            if let Some(seg) = p.segments.last() {
                if self.struct_field_names.contains_key(seg) {
                    return Some(seg.clone());
                }
            }
        }
        None
    }

    /// True when `te` is a leaf the f-string lowering can format directly: a
    /// primitive / String, or an all-unit enum (whose interpolation part is
    /// handled by `fstr_render_part` via `compile_unit_enum_display`).
    fn display_field_is_leaf(&self, te: &crate::ast::TypeExpr) -> bool {
        if let crate::ast::TypeKind::Path(p) = &te.kind {
            if let Some(seg) = p.segments.last() {
                return matches!(
                    seg.as_str(),
                    "i8" | "i16"
                        | "i32"
                        | "i64"
                        | "u8"
                        | "u16"
                        | "u32"
                        | "u64"
                        | "usize"
                        | "f32"
                        | "f64"
                        | "bool"
                        | "char"
                        | "String"
                ) || self.enum_unit_variants.contains_key(seg.as_str());
            }
        }
        false
    }

    /// Build the f-string parts for `base : type_name` — `TypeName { f: v, … }`
    /// in declaration order. Recurses for nested Display-struct fields.
    fn build_struct_display_parts(
        &self,
        base: &Expr,
        type_name: &str,
    ) -> Result<Vec<crate::ast::ParsedInterpolationPart>, String> {
        use crate::ast::ParsedInterpolationPart as P;
        let field_names = self
            .struct_field_names
            .get(type_name)
            .cloned()
            .ok_or_else(|| format!("Display: unknown struct '{type_name}'"))?;
        let field_tes = self
            .struct_field_type_exprs
            .get(type_name)
            .cloned()
            .unwrap_or_default();
        let mut parts: Vec<P> = vec![P::Text(format!("{type_name} {{ "))];
        for (i, fname) in field_names.iter().enumerate() {
            if i > 0 {
                parts.push(P::Text(", ".to_string()));
            }
            parts.push(P::Text(format!("{fname}: ")));
            let field_expr = Expr {
                kind: ExprKind::FieldAccess {
                    object: Box::new(base.clone()),
                    field: fname.clone(),
                },
                span: base.span.clone(),
            };
            let te = field_tes.get(i);
            match te.and_then(|t| self.display_field_struct_name(t)) {
                Some(nested) => {
                    parts.extend(self.build_struct_display_parts(&field_expr, &nested)?);
                }
                None => {
                    if te.map(|t| self.display_field_is_leaf(t)).unwrap_or(false) {
                        parts.push(P::Expr(Box::new(field_expr)));
                    } else {
                        let tdesc = te
                            .map(|t| format!("{:?}", t.kind))
                            .unwrap_or_else(|| "<unknown>".to_string());
                        return Err(format!(
                            "Display codegen for struct '{type_name}': field '{fname}' has a \
                             type ({tdesc}) whose Display is not yet supported under `karac build` \
                             (only primitives, String, and nested Display structs are supported; \
                             Vec/Map/Set/enum/tuple fields are tracked as subtask 5 follow-on)"
                        ));
                    }
                }
            }
        }
        parts.push(P::Text(" }".to_string()));
        Ok(parts)
    }

    /// Render a user-struct expression to an owning `String` value by
    /// compiling the synthetic f-string built from its fields.
    pub(super) fn compile_struct_display_string(
        &mut self,
        base: &Expr,
        type_name: &str,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let parts = self.build_struct_display_parts(base, type_name)?;
        let lit = Expr {
            kind: ExprKind::InterpolatedStringLit(parts),
            span: base.span.clone(),
        };
        self.compile_expr(&lit)
    }

    /// True when `value` as a let/assign RHS produces a String whose buffer is
    /// the staged `last_fstr_acc` — a direct f-string, or a user-struct
    /// `.to_string()` (which lowers via the synthetic f-string). The binding
    /// site must then consume `last_fstr_acc` so the accumulator's cleanup
    /// transfers to the new binding rather than double-freeing the buffer.
    pub(super) fn rhs_stages_fstr_acc(&self, value: &Expr) -> bool {
        match &value.kind {
            ExprKind::InterpolatedStringLit(_) => true,
            ExprKind::MethodCall {
                object,
                method,
                args,
                ..
            } if method == "to_string" && args.is_empty() => {
                self.expr_user_struct_name(object).is_some()
            }
            _ => false,
        }
    }

    /// If `expr` statically denotes a value of a known user struct type,
    /// return that struct's name. Covers the identifier and field-access
    /// receiver forms used at the `to_string` / f-string / println sites.
    /// If `expr`'s static type is a user struct/enum carrying a user
    /// `impl Display` — i.e. a compiled `<Type>.to_string` method, as opposed
    /// to the built-in `display_render` renderer or a `#[derive(Display)]` —
    /// return that type name. Used to route Display positions (`x.to_string()`,
    /// `f"{x}"`, `println(x)`) to the user method instead of the synthesized
    /// built-in. The discriminator is the function name: only a user impl
    /// produces a `<Type>.to_string` LLVM function (built-ins are
    /// `karac_display_<T>`). GAP-W4.
    pub(super) fn user_display_impl_type(&self, expr: &Expr) -> Option<String> {
        let tn = self
            .expr_user_struct_name(expr)
            .or_else(|| self.expr_user_enum_name_any(expr))?;
        if self
            .module
            .get_function(&format!("{tn}.to_string"))
            .is_some()
        {
            Some(tn)
        } else {
            None
        }
    }

    pub(super) fn expr_user_struct_name(&self, expr: &Expr) -> Option<String> {
        match &expr.kind {
            ExprKind::Identifier(n) => self
                .var_type_names
                .get(n.as_str())
                .filter(|tn| self.struct_field_names.contains_key(tn.as_str()))
                .cloned(),
            ExprKind::FieldAccess { object, field } => {
                let outer = self.expr_user_struct_name(object)?;
                let tes = self.struct_field_type_exprs.get(&outer)?;
                let names = self.struct_field_names.get(&outer)?;
                let idx = names.iter().position(|f| f == field)?;
                self.display_field_struct_name(tes.get(idx)?)
            }
            _ => None,
        }
    }

    /// If `expr` statically denotes a value of a known all-unit user enum,
    /// return that enum's name. Same place-expression coverage (identifier /
    /// field access) as `expr_user_struct_name`.
    pub(super) fn expr_user_enum_name(&self, expr: &Expr) -> Option<String> {
        match &expr.kind {
            ExprKind::Identifier(n) => self
                .var_type_names
                .get(n.as_str())
                .filter(|tn| self.enum_unit_variants.contains_key(tn.as_str()))
                .cloned(),
            ExprKind::FieldAccess { object, field } => {
                let outer = self.expr_user_struct_name(object)?;
                let tes = self.struct_field_type_exprs.get(&outer)?;
                let names = self.struct_field_names.get(&outer)?;
                let idx = names.iter().position(|f| f == field)?;
                if let crate::ast::TypeKind::Path(p) = &tes.get(idx)?.kind {
                    if let Some(seg) = p.segments.last() {
                        if self.enum_unit_variants.contains_key(seg) {
                            return Some(seg.clone());
                        }
                    }
                }
                None
            }
            _ => None,
        }
    }

    /// Render an all-unit enum value to `(ptr, len)` of its variant name: load
    /// the tag (field 0) and fold a select-chain over per-variant name globals.
    /// The first variant is the default (its tag needs no select, since the
    /// tag is always one of the exhaustive 0..N range).
    pub(super) fn compile_unit_enum_display(
        &mut self,
        enum_expr: &Expr,
        enum_name: &str,
    ) -> Result<(PointerValue<'ctx>, inkwell::values::IntValue<'ctx>), String> {
        let variants = self
            .enum_unit_variants
            .get(enum_name)
            .cloned()
            .ok_or_else(|| format!("Display: '{enum_name}' is not an all-unit enum"))?;
        // A standalone all-unit enum value is a `{ i64 }` struct (tag at field
        // 0); the same enum embedded as a struct field is stored as the bare
        // `i64` tag (the single-word `{i64}` wrapper is collapsed). Accept
        // both shapes.
        let val = self.compile_expr(enum_expr)?;
        let tag = match val {
            BasicValueEnum::IntValue(iv) => iv,
            BasicValueEnum::StructValue(sv) => self
                .builder
                .build_extract_value(sv, 0, "enum.tag")
                .unwrap()
                .into_int_value(),
            other => {
                return Err(format!(
                    "Display: enum '{enum_name}' value has unexpected representation {other:?}"
                ))
            }
        };
        let i64_t = self.context.i64_type();
        let mut acc: Option<(PointerValue<'ctx>, inkwell::values::IntValue<'ctx>)> = None;
        for vname in &variants {
            let tagval = *self
                .enum_layouts
                .get(enum_name)
                .and_then(|l| l.tags.get(vname))
                .ok_or_else(|| format!("Display: missing tag for {enum_name}.{vname}"))?;
            let g = self
                .builder
                .build_global_string_ptr(vname, "enumv")
                .unwrap()
                .as_pointer_value();
            let l = i64_t.const_int(vname.len() as u64, false);
            acc = Some(match acc {
                None => (g, l),
                Some((ap, al)) => {
                    let is_v = self
                        .builder
                        .build_int_compare(
                            inkwell::IntPredicate::EQ,
                            tag,
                            i64_t.const_int(tagval, false),
                            "enum.is",
                        )
                        .unwrap();
                    let p = self
                        .builder
                        .build_select(is_v, g, ap, "enum.psel")
                        .unwrap()
                        .into_pointer_value();
                    let len = self
                        .builder
                        .build_select(is_v, l, al, "enum.lsel")
                        .unwrap()
                        .into_int_value();
                    (p, len)
                }
            });
        }
        acc.ok_or_else(|| format!("Display: enum '{enum_name}' has no variants"))
    }

    /// Emit (or reuse) a value-driven, buffer-append Display function for a
    /// user enum that may carry payload variants (tuple or struct) — the
    /// payload-enum extension of `compile_unit_enum_display` (which is
    /// all-unit-only and select-chain-based). Signature
    /// `void karac_display_<enum>(ptr val_ptr, ptr acc)`: load the tag, switch
    /// per variant, append the variant name, and for payload variants
    /// reconstruct each field READ-ONLY from the unified payload words (via
    /// `EnumLayout.field_word_offsets` — the same extraction
    /// `bind_pattern_values` uses) and recurse through
    /// `emit_display_fn_for_type_expr`. Read-only is load-bearing: Display must
    /// not move/free a heap payload (e.g. `IoError.Other(String)`) — we render
    /// the borrowed buffer and never register a drop, mirroring how
    /// `emit_vec_display_body` reads elements without consuming. Format matches
    /// the interpreter's `Value::EnumVariant` Display: `Variant` /
    /// `Variant(f0, f1)` / `Variant { name: v }`.
    pub(super) fn emit_enum_display_fn(&mut self, enum_name: &str) -> FunctionValue<'ctx> {
        let cache_key = enum_name.to_string();
        if let Some(&f) = self.display_fn_cache.get(&cache_key) {
            return f;
        }
        let fn_name = format!("karac_display_{enum_name}");
        if let Some(f) = self.module.get_function(&fn_name) {
            self.display_fn_cache.insert(cache_key, f);
            return f;
        }

        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();

        // Snapshot the enum's variant shapes (name + VariantKind) from the
        // program AST and its layout (tags + per-field word offsets) up front,
        // so the per-variant emission below can borrow `self` mutably.
        let collect_variants = |items: &[Item]| -> Option<Vec<(String, VariantKind)>> {
            items.iter().find_map(|it| match it {
                Item::EnumDef(e) if e.name == enum_name => Some(
                    e.variants
                        .iter()
                        .map(|v| (v.name.clone(), v.kind.clone()))
                        .collect::<Vec<_>>(),
                ),
                _ => None,
            })
        };
        let variants: Vec<(String, VariantKind)> = self
            .program_snapshot
            .as_ref()
            .and_then(|p| collect_variants(&p.items))
            // A baked-stdlib enum (`IoError`, `VarError`) is never in the user
            // `program_snapshot` — its variant shapes live only in
            // `STDLIB_PROGRAMS`. Without this fallback the variant set is empty,
            // the switch gets zero cases, and `#[derive(Display)]` renders the
            // tag (or nothing) instead of the variant. The seeded layout above
            // supplies the tags/offsets; this supplies the names + kinds.
            .or_else(|| {
                crate::prelude::STDLIB_PROGRAMS
                    .iter()
                    .find_map(|(_, p)| collect_variants(&p.items))
            })
            .unwrap_or_default();
        let layout = self
            .enum_layouts
            .get(enum_name)
            .expect("emit_enum_display_fn: enum has no layout");
        let llvm_ty = layout.llvm_type;
        let tags = layout.tags.clone();
        let field_offsets = layout.field_word_offsets.clone();

        let saved_bb = self.builder.get_insert_block();
        let saved_fn = self.current_fn;

        let display_fn_ty = self
            .context
            .void_type()
            .fn_type(&[ptr_ty.into(), ptr_ty.into()], false);
        let display_fn = self
            .module
            .add_function(&fn_name, display_fn_ty, Some(Linkage::Internal));
        self.display_fn_cache
            .insert(enum_name.to_string(), display_fn);

        let entry_bb = self.context.append_basic_block(display_fn, "entry");
        self.builder.position_at_end(entry_bb);
        self.current_fn = Some(display_fn);
        let val_ptr = display_fn.get_nth_param(0).unwrap().into_pointer_value();
        let acc = display_fn.get_nth_param(1).unwrap().into_pointer_value();

        // Load the whole enum aggregate so payload words extract by index
        // (field 0 = tag, fields 1.. = payload words) — same shape the
        // pattern-binding path reads.
        let agg = self
            .builder
            .build_load(llvm_ty, val_ptr, "enum.agg")
            .unwrap()
            .into_struct_value();
        let tag = self
            .builder
            .build_extract_value(agg, 0, "enum.tag")
            .unwrap()
            .into_int_value();

        let exit_bb = self.context.append_basic_block(display_fn, "enum.exit");
        let default_bb = self.context.append_basic_block(display_fn, "enum.default");

        // One block per variant, dispatched by a switch on the tag.
        let mut cases: Vec<(
            inkwell::values::IntValue<'ctx>,
            inkwell::basic_block::BasicBlock<'ctx>,
        )> = Vec::with_capacity(variants.len());
        let mut variant_blocks: Vec<inkwell::basic_block::BasicBlock<'ctx>> = Vec::new();
        for (vname, _) in &variants {
            let bb = self
                .context
                .append_basic_block(display_fn, &format!("enum.v.{vname}"));
            variant_blocks.push(bb);
            let tagval = tags.get(vname).copied().unwrap_or(0);
            cases.push((i64_t.const_int(tagval, false), bb));
        }
        self.builder.build_switch(tag, default_bb, &cases).unwrap();

        // Exhaustive over the declared variants — the tag is always one of them.
        self.builder.position_at_end(default_bb);
        self.builder.build_unreachable().unwrap();

        for (idx, (vname, kind)) in variants.iter().enumerate() {
            self.builder.position_at_end(variant_blocks[idx]);
            self.disp_append_lit(acc, vname);
            let offsets = field_offsets.get(vname).cloned().unwrap_or_default();
            match kind {
                VariantKind::Unit => {}
                VariantKind::Tuple(field_tes) => {
                    self.disp_append_lit(acc, "(");
                    for (i, field_te) in field_tes.iter().enumerate() {
                        if i > 0 {
                            self.disp_append_lit(acc, ", ");
                        }
                        self.emit_enum_field_display(agg, &offsets, i, field_te, acc, display_fn);
                    }
                    self.disp_append_lit(acc, ")");
                }
                VariantKind::Struct(fields) => {
                    self.disp_append_lit(acc, " { ");
                    for (i, sf) in fields.iter().enumerate() {
                        if i > 0 {
                            self.disp_append_lit(acc, ", ");
                        }
                        self.disp_append_lit(acc, &format!("{}: ", sf.name));
                        self.emit_enum_field_display(agg, &offsets, i, &sf.ty, acc, display_fn);
                    }
                    self.disp_append_lit(acc, " }");
                }
            }
            // An append may have split the current block (buffer grow); branch
            // to exit from wherever we ended up.
            self.builder.build_unconditional_branch(exit_bb).unwrap();
        }

        self.builder.position_at_end(exit_bb);
        self.builder.build_return(None).unwrap();

        self.current_fn = saved_fn;
        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        display_fn
    }

    /// Render one enum payload field (declaration index `i`) into `acc`:
    /// extract its READ-ONLY payload words from the loaded enum aggregate
    /// `agg` using `offsets[i] = (start_word, num_words)`, rebuild the
    /// source-typed field value (no copy / no drop registration), spill it to
    /// a stack slot, and call the field type's append-form Display fn. Helper
    /// for `emit_enum_display_fn`.
    fn emit_enum_field_display(
        &mut self,
        agg: inkwell::values::StructValue<'ctx>,
        offsets: &[(usize, usize)],
        i: usize,
        field_te: &TypeExpr,
        acc: PointerValue<'ctx>,
        display_fn: FunctionValue<'ctx>,
    ) {
        let i64_t = self.context.i64_type();
        let zero = i64_t.const_zero();
        let (start, num) = offsets.get(i).copied().unwrap_or((i, 1));
        let word = |s: &Self, j: usize| -> inkwell::values::IntValue<'ctx> {
            if j < num {
                s.builder
                    .build_extract_value(agg, (start + j + 1) as u32, "enum.fw")
                    .unwrap()
                    .into_int_value()
            } else {
                zero
            }
        };
        let w0 = word(self, 0);
        let w1 = word(self, 1);
        let w2 = word(self, 2);
        let field_ty = self.llvm_type_for_type_expr(field_te);
        let field_val = self
            .rebuild_value_from_payload_words(field_ty, w0, w1, w2)
            .unwrap_or_else(|_| w0.into());
        let slot = self.create_entry_alloca(display_fn, "enum.field", field_val.get_type());
        self.builder.build_store(slot, field_val).unwrap();
        let field_disp = self.emit_display_fn_for_type_expr(field_te);
        self.builder
            .build_call(field_disp, &[slot.into(), acc.into()], "enum.fd")
            .unwrap();
    }

    /// If `expr` statically denotes a value of a user enum that
    /// `emit_enum_display_fn` can render — any declared enum with a layout
    /// EXCEPT the bespoke-Display built-ins (`Option`/`Result` are generic +
    /// have dedicated handling; other seeded enums route through their own
    /// paths) — return that enum's name. The payload-bearing sibling of
    /// `expr_user_enum_name` (which is all-unit-only). Same place-expression
    /// coverage (identifier / field access).
    pub(super) fn expr_user_enum_name_any(&self, expr: &Expr) -> Option<String> {
        let tn = match &expr.kind {
            ExprKind::Identifier(n) => self.var_type_names.get(n.as_str()).cloned(),
            ExprKind::FieldAccess { object, field } => {
                let outer = self.expr_user_struct_name(object)?;
                let tes = self.struct_field_type_exprs.get(&outer)?;
                let names = self.struct_field_names.get(&outer)?;
                let idx = names.iter().position(|f| f == field)?;
                if let crate::ast::TypeKind::Path(p) = &tes.get(idx)?.kind {
                    p.segments.last().cloned()
                } else {
                    None
                }
            }
            _ => None,
        }?;
        if self.enum_layouts.contains_key(&tn)
            && (!self.seeded_enum_names.contains(&tn)
                || self.baked_display_enum_names.contains(&tn))
            && tn != "Option"
            && tn != "Result"
        {
            Some(tn)
        } else {
            None
        }
    }

    /// Render a user-enum value `expr` via its value-driven Display fn
    /// (`emit_enum_display_fn`) into a fresh String accumulator; return
    /// `(acc_alloca, loaded String value)` — the same shape
    /// `render_via_display_fn` returns for collections. Resolves the enum
    /// value's address: a bound identifier uses its alloca directly (read-only,
    /// no copy — Display never consumes the value); any other expression is
    /// compiled and spilled to a stack slot. Used by the f-string / `println` /
    /// `to_string` enum dispatch and the `main() -> Result` Err exit.
    pub(super) fn render_user_enum_display(
        &mut self,
        expr: &Expr,
        enum_name: &str,
    ) -> Result<(PointerValue<'ctx>, BasicValueEnum<'ctx>), String> {
        let disp = self.emit_enum_display_fn(enum_name);
        let val_ptr = if let ExprKind::Identifier(n) = &expr.kind {
            self.variables.get(n.as_str()).map(|s| s.ptr)
        } else {
            None
        };
        let val_ptr = match val_ptr {
            Some(p) => p,
            None => {
                let val = self.compile_expr(expr)?;
                let fn_val = self.current_fn.unwrap();
                let slot = self.create_entry_alloca(fn_val, "enum.disp.tmp", val.get_type());
                self.builder.build_store(slot, val).unwrap();
                slot
            }
        };
        Ok(self.render_via_display_fn(disp, val_ptr))
    }

    /// Render `value_ptr` via the append-form display fn `disp` into a fresh
    /// String accumulator; return `(acc_alloca, loaded String value)`. The
    /// caller owns the heap buffer — free it inline (println) or `track_vec_var`
    /// the alloca (f-string) / let the binding own it (to_string).
    pub(super) fn render_via_display_fn(
        &mut self,
        disp: FunctionValue<'ctx>,
        value_ptr: PointerValue<'ctx>,
    ) -> (PointerValue<'ctx>, BasicValueEnum<'ctx>) {
        let vec_ty = self.vec_struct_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();
        let fn_val = self.current_fn.unwrap();
        let acc = self.create_entry_alloca(fn_val, "cd.acc", vec_ty.into());
        // Init {null, 0, 0} at the use site (the alloca lives in the entry
        // block; re-init each time this point executes — loop-safe).
        let dpp = self
            .builder
            .build_struct_gep(vec_ty, acc, 0, "cd.dpp")
            .unwrap();
        let lp = self
            .builder
            .build_struct_gep(vec_ty, acc, 1, "cd.lp")
            .unwrap();
        let cp = self
            .builder
            .build_struct_gep(vec_ty, acc, 2, "cd.cp")
            .unwrap();
        self.builder.build_store(dpp, ptr_ty.const_null()).unwrap();
        self.builder.build_store(lp, i64_t.const_zero()).unwrap();
        self.builder.build_store(cp, i64_t.const_zero()).unwrap();
        self.builder
            .build_call(disp, &[value_ptr.into(), acc.into()], "cd.call")
            .unwrap();
        let val = self.builder.build_load(vec_ty, acc, "cd.val").unwrap();
        (acc, val)
    }

    /// If `expr` is an identifier bound to a `Vec`/`Map`/`Set`, render it via
    /// its append-form Display fn and return `(acc_alloca, String value)`.
    /// Detection mirrors `compile_print`'s collection arms. `None` for any
    /// other expression (caller falls back). Used by collection f-string
    /// interpolation and `to_string`.
    pub(super) fn try_compile_collection_display(
        &mut self,
        expr: &Expr,
    ) -> Result<Option<(PointerValue<'ctx>, BasicValueEnum<'ctx>)>, String> {
        let ExprKind::Identifier(name) = &expr.kind else {
            return Ok(None);
        };
        let name = name.clone();
        let Some(slot) = self.variables.get(name.as_str()).copied() else {
            return Ok(None);
        };
        // Vec[T] — `vec_elem_types` + `var_elem_type_exprs` (String sets only
        // the former). Checked before Map since Map lacks `vec_elem_types`.
        if self.vec_elem_types.contains_key(&name) && self.var_elem_type_exprs.contains_key(&name) {
            let elem_te = self.var_elem_type_exprs[&name].clone();
            let disp = self.emit_vec_display_fn_te(&elem_te);
            return Ok(Some(self.render_via_display_fn(disp, slot.ptr)));
        }
        if self.map_key_type_exprs.contains_key(&name)
            && self.var_elem_type_exprs.contains_key(&name)
        {
            let k = self.map_key_type_exprs[&name].clone();
            let v = self.var_elem_type_exprs[&name].clone();
            let disp = self.emit_map_display_fn(&k, &v);
            return Ok(Some(self.render_via_display_fn(disp, slot.ptr)));
        }
        if self.set_elem_type_exprs.contains_key(&name) {
            let elem_te = self.set_elem_type_exprs[&name].clone();
            let disp = self.emit_set_display_fn(&elem_te);
            return Ok(Some(self.render_via_display_fn(disp, slot.ptr)));
        }
        Ok(None)
    }
}
