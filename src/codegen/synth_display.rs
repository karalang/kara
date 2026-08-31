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
use inkwell::values::{
    BasicMetadataValueEnum, BasicValue, BasicValueEnum, FunctionValue, PointerValue,
};
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
    /// Append the `Debug` spelling of a `String` / `str` leaf — quoted and
    /// escaped, e.g. `"a\nb"` — to the accumulator, then free the runtime's
    /// temporary buffer (B-2026-08-23-18).
    ///
    /// The escaping is Rust's own `{:?}`, performed inside
    /// `karac_dbg_quote_str`, which is what makes this byte-identical to the
    /// interpreter's `format!("{:?}", s)` rather than merely similar to it.
    pub(super) fn disp_append_debug_str(
        &mut self,
        acc: PointerValue<'ctx>,
        data: PointerValue<'ctx>,
        len: inkwell::values::IntValue<'ctx>,
    ) {
        let i64_t = self.context.i64_type();
        let out_len = self.builder.build_alloca(i64_t, "dbgq.s.outlen").unwrap();
        let f = self.runtime_fns.karac_dbg_quote_str_fn;
        let call = self
            .builder
            .build_call(f, &[data.into(), len.into(), out_len.into()], "dbgq.s")
            .unwrap();
        let quoted = call
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        let qlen = self
            .builder
            .build_load(i64_t, out_len, "dbgq.s.len")
            .unwrap()
            .into_int_value();
        self.emit_string_append_raw(acc, quoted, qlen);
        // The quote buffer is a one-shot temporary; the accumulator copied the
        // bytes. Freeing here keeps a `dbg` of a String off LeakSanitizer's
        // report (the Linux `memory-sanitizer` job is the authoritative gate).
        self.builder
            .build_call(self.runtime_fns.free_fn, &[quoted.into()], "")
            .unwrap();
    }

    /// `Debug` spelling of a `char` leaf — `'x'`, `'\n'`, `'\u{1f600}'`.
    /// Same runtime-owned-escaping rationale as `disp_append_debug_str`.
    pub(super) fn disp_append_debug_char(
        &mut self,
        acc: PointerValue<'ctx>,
        cp: inkwell::values::IntValue<'ctx>,
    ) {
        let i32_t = self.context.i32_type();
        let i64_t = self.context.i64_type();
        let cp32 = match cp.get_type().get_bit_width() {
            32 => cp,
            w if w < 32 => self
                .builder
                .build_int_z_extend(cp, i32_t, "dbgq.c.z")
                .unwrap(),
            _ => self
                .builder
                .build_int_truncate(cp, i32_t, "dbgq.c.t")
                .unwrap(),
        };
        let out_len = self.builder.build_alloca(i64_t, "dbgq.c.outlen").unwrap();
        let f = self.runtime_fns.karac_dbg_quote_char_fn;
        let call = self
            .builder
            .build_call(f, &[cp32.into(), out_len.into()], "dbgq.c")
            .unwrap();
        let quoted = call
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        let qlen = self
            .builder
            .build_load(i64_t, out_len, "dbgq.c.len")
            .unwrap()
            .into_int_value();
        self.emit_string_append_raw(acc, quoted, qlen);
        self.builder
            .build_call(self.runtime_fns.free_fn, &[quoted.into()], "")
            .unwrap();
    }

    /// Look up a synthesized renderer in the cache for the CURRENT render mode
    /// (`Display` or `Debug`). B-2026-08-23-18 — see `DisplayState::debug_fn_cache`
    /// for why the two modes may not share one cache.
    pub(super) fn disp_cache_get(&self, key: &str) -> Option<FunctionValue<'ctx>> {
        if self.display.debug_render {
            self.display.debug_fn_cache.get(key).copied()
        } else {
            self.display.display_fn_cache.get(key).copied()
        }
    }

    /// Record a synthesized renderer in the current render mode's cache.
    pub(super) fn disp_cache_put(&mut self, key: String, f: FunctionValue<'ctx>) {
        if self.display.debug_render {
            self.display.debug_fn_cache.insert(key, f);
        } else {
            self.display.display_fn_cache.insert(key, f);
        }
    }

    /// Symbol-name prefix for the current render mode. The two families must
    /// not collide in the module's symbol table either — `get_function` is
    /// consulted as a second-level cache, so a shared prefix would let a
    /// `Display` function satisfy a `Debug` lookup across a cache miss.
    pub(super) fn disp_sym_prefix(&self) -> &'static str {
        if self.display.debug_render {
            "karac_debug_"
        } else {
            "karac_display_"
        }
    }

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
                self.runtime_fns.snprintf_fn,
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
        if let Some(f) = self.disp_cache_get(type_name) {
            return f;
        }
        let fn_name = format!("{}{type_name}", self.disp_sym_prefix());
        if let Some(f) = self.module.get_function(&fn_name) {
            self.display
                .display_fn_cache
                .insert(type_name.to_string(), f);
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
        self.display
            .display_fn_cache
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
            // The 128-bit widths cannot ride the `%lld` / `%llu` arms above:
            // both extend to i64 first, which truncates. They go through the
            // runtime's own 128-bit formatter — the same one the scalar
            // `println` path uses, so a `u128` renders identically whether it
            // is printed directly or reached through a `Vec` / `Option`.
            //
            // Without an arm here `emit_display_fn_for_type` fell to its
            // catch-all and PANICKED ("type_name 'u128' not yet supported"), so
            // `println(v)` on a `Vec[i128]` aborted the compiler rather than
            // compiling (B-2026-08-19-23).
            "i128" | "u128" => {
                let v = self
                    .builder
                    .build_load(ty, val_ptr, "v")
                    .unwrap()
                    .into_int_value();
                let (buf_ptr, len) = self.format_i128_to_stack_buf(v, type_name == "u128");
                self.emit_string_append_raw(acc, buf_ptr, len);
            }
            // B-2026-08-31-10 — f16/bf16 belong on this arm too. They were
            // absent, so an `Option[f16]` reaching the type-directed renderer
            // fell to the catch-all and PANICKED the compiler. The formatter
            // widens any narrower float to f64 itself (routing bf16 through
            // f32, per B-2026-07-22-1), so the arm needs no width-specific
            // work — exactly as it needed none for f32.
            "f32" | "f64" | "f16" | "bf16" => {
                // Render with Rust's shortest-round-trip `{}` (via the runtime
                // formatter) so a struct's `Display` prints floats identically
                // to the interpreter — not C `%g`. `format_f64_to_stack_buf`
                // widens narrower floats to f64 itself.
                let v = self
                    .builder
                    .build_load(ty, val_ptr, "v")
                    .unwrap()
                    .into_float_value();
                let (buf_ptr, len) = self.format_f64_to_stack_buf(v);
                self.emit_string_append_raw(acc, buf_ptr, len);
            }
            // Total-order float wrappers (B-2026-08-11-8). Render the INNER
            // float exactly as the bare primitive would — `1.5`, not
            // `F64(1.5)`. The wrapper exists to supply `Eq`/`Ord`/`Hash`, not
            // a distinct textual form; a percentile report that sorts through
            // `F64` should print the numbers, not the wrapper. (Java's
            // `Double` prints the same as `double` for the same reason.)
            // Reuses the `f32`/`f64` shortest-round-trip formatter below, so
            // wrapped and unwrapped renderings cannot drift apart.
            // B-2026-08-27-11 — the name alone is not enough: a user type of
            // this name is not the wrapper and must render as the struct they
            // wrote. Reached here for a shadowed name, this arm formats the
            // value as the wrapped FLOAT, so a `bool` (the result of comparing
            // two user `F64`s, interpolated in an f-string) printed as `0`
            // where the interpreter printed `false`.
            _ if self.is_prelude_total_float_wrapper(type_name) => {
                let inner_ty: BasicTypeEnum<'ctx> = match type_name {
                    "F32" => self.context.f32_type().into(),
                    "F64" => self.context.f64_type().into(),
                    "F16" => self.context.f16_type().into(),
                    _ => self.context.bf16_type().into(),
                };
                let st = self.context.struct_type(&[inner_ty], false);
                let loaded = self
                    .builder
                    .build_load(st, val_ptr, "tfw")
                    .unwrap()
                    .into_struct_value();
                let v = self
                    .builder
                    .build_extract_value(loaded, 0, "tfw.inner")
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
                // `Debug` quotes and escapes the char; `Display` emits the bare
                // glyph. One of the two leaves where the modes diverge.
                if self.display.debug_render {
                    self.disp_append_debug_char(acc, v);
                } else {
                    let (p, l) = self.emit_codepoint_to_utf8(v);
                    self.emit_string_append_raw(acc, p, l);
                }
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
                // The other diverging leaf: `Debug` quotes and escapes.
                if self.display.debug_render {
                    self.disp_append_debug_str(acc, data, len);
                } else {
                    self.emit_string_append_raw(acc, data, len);
                }
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

    /// B-2026-08-31-19 — the `Array[T, N]` twin of [`Self::emit_vec_display_fn_te`].
    ///
    /// Codegen had no array Display AT ANY DEPTH, and the two halves failed
    /// differently: nested (a `Vec` element, a struct field, a tuple field) it
    /// fell through `emit_display_fn_for_type_expr` to the by-name catch-all
    /// and PANICKED the compiler; at depth 0 it was silent, printing the first
    /// element for `f"{a}"` and nothing at all for `println(a)` — and for an
    /// `Array[String, 2]` it printed the element's raw data POINTER. The
    /// interpreter renders all of these `[1, 2, 3]`.
    ///
    /// Shares `emit_vec_display_body`'s text and loop shape, because an array
    /// prints exactly like a `Vec` and the two must not drift. The layout
    /// differs in one way only: an array's elements are INLINE at `val_ptr`
    /// rather than behind the `{ptr, len, cap}` triple's data pointer, and its
    /// length is a compile-time constant instead of a loaded field. Emitted as
    /// a loop rather than unrolled so a large extent does not inflate the
    /// module.
    ///
    /// Cached under `display_mangle_te`'s name for the whole `Array[T, N]`,
    /// which includes the extent — `Array[i64, 2]` and `Array[i64, 3]` are
    /// different layouts and must not collapse onto one body (B-2026-08-27-25,
    /// the same reason the `Vector` arm keys on its lane count).
    pub(super) fn emit_array_display_fn(
        &mut self,
        elem_te: &TypeExpr,
        len: u64,
        array_te: &TypeExpr,
    ) -> FunctionValue<'ctx> {
        let type_name = Self::display_mangle_te(array_te);
        if let Some(f) = self.disp_cache_get(&type_name) {
            return f;
        }
        let fn_name = format!("{}{type_name}", self.disp_sym_prefix());
        if let Some(f) = self.module.get_function(&fn_name) {
            self.disp_cache_put(type_name, f);
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
        self.disp_cache_put(type_name, display_fn);

        let entry_bb = self.context.append_basic_block(display_fn, "entry");
        self.builder.position_at_end(entry_bb);
        self.current_fn = Some(display_fn);
        let val_ptr = display_fn.get_nth_param(0).unwrap().into_pointer_value();
        let acc = display_fn.get_nth_param(1).unwrap().into_pointer_value();

        self.emit_array_display_body(display_fn, val_ptr, acc, elem_te, len);

        self.builder.build_return(None).unwrap();

        self.current_fn = saved_fn;
        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        display_fn
    }

    /// Body of [`Self::emit_array_display_fn`] — `[e0, e1, …]` over `len`
    /// elements laid out inline from `val_ptr`. Split out so the ELEMENT
    /// dispatch is one call to `emit_display_fn_for_type_expr`, which is what
    /// makes `Array[Vec[i64], 2]`, `Array[String, 2]` and nested arrays
    /// compose without any per-shape work here.
    fn emit_array_display_body(
        &mut self,
        display_fn: FunctionValue<'ctx>,
        val_ptr: PointerValue<'ctx>,
        acc: PointerValue<'ctx>,
        elem_te: &TypeExpr,
        len: u64,
    ) {
        let i8_t = self.context.i8_type();
        let i64_t = self.context.i64_type();
        let elem_ty = self.llvm_type_for_type_expr(elem_te);

        // Materialize the element Display fn BEFORE emitting this body: the
        // recursive emit moves the builder's insert block, and the dispatcher
        // restores only the caller's position. Same ordering constraint the
        // Vec body documents.
        let elem_disp = self.emit_display_fn_for_type_expr(elem_te);

        self.disp_append_lit(acc, "[");

        let raw_size = elem_ty
            .size_of()
            .unwrap_or_else(|| i64_t.const_int(8, false));
        let elem_size = if raw_size.get_type().get_bit_width() == 64 {
            raw_size
        } else {
            self.builder
                .build_int_z_extend(raw_size, i64_t, "aesz64")
                .unwrap()
        };
        let n = i64_t.const_int(len, false);

        let pre_bb = self.builder.get_insert_block().unwrap();
        let hdr_bb = self.context.append_basic_block(display_fn, "arr.hdr");
        let bdy_bb = self.context.append_basic_block(display_fn, "arr.bdy");
        let sep_bb = self.context.append_basic_block(display_fn, "arr.sep");
        let elem_bb = self.context.append_basic_block(display_fn, "arr.elem");
        let exit_bb = self.context.append_basic_block(display_fn, "arr.exit");

        self.builder.build_unconditional_branch(hdr_bb).unwrap();

        self.builder.position_at_end(hdr_bb);
        let i_phi = self.builder.build_phi(i64_t, "arr.i").unwrap();
        i_phi.add_incoming(&[(&i64_t.const_zero(), pre_bb)]);
        let i_val = i_phi.as_basic_value().into_int_value();
        let cond = self
            .builder
            .build_int_compare(IntPredicate::ULT, i_val, n, "arr.cond")
            .unwrap();
        self.builder
            .build_conditional_branch(cond, bdy_bb, exit_bb)
            .unwrap();

        self.builder.position_at_end(bdy_bb);
        let is_first = self
            .builder
            .build_int_compare(IntPredicate::EQ, i_val, i64_t.const_zero(), "arr.is.first")
            .unwrap();
        self.builder
            .build_conditional_branch(is_first, elem_bb, sep_bb)
            .unwrap();

        self.builder.position_at_end(sep_bb);
        self.disp_append_lit(acc, ", ");
        self.builder.build_unconditional_branch(elem_bb).unwrap();

        self.builder.position_at_end(elem_bb);
        let offset = self
            .builder
            .build_int_mul(i_val, elem_size, "arr.off")
            .unwrap();
        // The elements are INLINE — index from `val_ptr` itself, where the Vec
        // body indexes from the loaded data pointer.
        let elem_ptr = unsafe {
            self.builder
                .build_gep(i8_t, val_ptr, &[offset], "arr.elem.p")
                .unwrap()
        };
        self.builder
            .build_call(elem_disp, &[elem_ptr.into(), acc.into()], "aed")
            .unwrap();
        let i_next = self
            .builder
            .build_int_add(i_val, i64_t.const_int(1, false), "arr.i1")
            .unwrap();
        let elem_end_bb = self.builder.get_insert_block().unwrap();
        i_phi.add_incoming(&[(&i_next, elem_end_bb)]);
        self.builder.build_unconditional_branch(hdr_bb).unwrap();

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
        if let Some(f) = self.disp_cache_get(&type_name) {
            return f;
        }
        let fn_name = format!("{}{type_name}", self.disp_sym_prefix());
        if let Some(f) = self.module.get_function(&fn_name) {
            self.disp_cache_put(type_name, f);
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
        self.disp_cache_put(type_name, display_fn);

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
            .build_call(
                self.runtime_fns.karac_map_iter_new_fn,
                &[map_handle.into()],
                "md.iter",
            )
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
                self.runtime_fns.karac_map_iter_next_fn,
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
            .build_call(
                self.runtime_fns.karac_map_iter_free_fn,
                &[iter_final.into()],
                "",
            )
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
        if let Some(f) = self.disp_cache_get(&type_name) {
            return f;
        }
        let fn_name = format!("{}{type_name}", self.disp_sym_prefix());
        if let Some(f) = self.module.get_function(&fn_name) {
            self.disp_cache_put(type_name, f);
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
        self.disp_cache_put(type_name, display_fn);

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
            .build_call(
                self.runtime_fns.karac_map_iter_new_fn,
                &[set_handle.into()],
                "sd.iter",
            )
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
                self.runtime_fns.karac_map_iter_next_fn,
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
            .build_call(
                self.runtime_fns.karac_map_iter_free_fn,
                &[iter_final.into()],
                "",
            )
            .unwrap();
        self.disp_append_lit(acc, "}");
    }

    /// Emit (or reuse) a Display function for `SortedMap[K, V]`
    /// (B-2026-08-14-35). The sorted siblings share `Map`/`Set`'s `KaracMap`
    /// storage, so before this existed they were pointed at the unsorted
    /// renderers above — and inherited both of that renderer's assumptions:
    /// the literal `{` / `Set{` prefix, and HASH-BUCKET iteration order. A
    /// compiled `SortedMap{apple: 2, banana: 4, mango: 3, zebra: 1}` printed
    /// `{zebra: 1, apple: 2, banana: 4, mango: 3}` — a different type name over
    /// a different order, against an interpreter that got both right.
    ///
    /// Order comes from the same `emit_sorted_keys_buf` the `for` loop and
    /// `keys()` already use, so the render cannot drift from iteration: walk
    /// the sorted key buffer by index and look each value up with
    /// `karac_map_get`. That is why this returns `Result` where the unsorted
    /// entry points do not — `emit_sorted_key_cmp_fn` declines element types
    /// codegen cannot order, and rendering those in bucket order under a
    /// `SortedMap` label would be a worse bug than the one being fixed.
    pub(super) fn emit_sorted_map_display_fn(
        &mut self,
        key_te: &TypeExpr,
        val_te: &TypeExpr,
    ) -> Result<FunctionValue<'ctx>, String> {
        let key_name = Self::display_mangle_te(key_te);
        let val_name = Self::display_mangle_te(val_te);
        let type_name = format!("SortedMap_{key_name}_{val_name}");
        self.emit_sorted_collection_display_fn(type_name, key_te, Some(val_te), "SortedMap{")
    }

    /// `SortedSet[T]`'s Display fn — the `Set` sibling of
    /// `emit_sorted_map_display_fn`, minus the value lookup (a Set lowers to
    /// `Map[T, ()]`, so the sorted key buffer IS the element sequence).
    pub(super) fn emit_sorted_set_display_fn(
        &mut self,
        elem_te: &TypeExpr,
    ) -> Result<FunctionValue<'ctx>, String> {
        let elem_name = Self::display_mangle_te(elem_te);
        let type_name = format!("SortedSet_{elem_name}");
        self.emit_sorted_collection_display_fn(type_name, elem_te, None, "SortedSet{")
    }

    /// Shared emitter behind the two entry points above: same prologue,
    /// cache and calling convention as `emit_map_display_fn`, with the body
    /// driven by the sorted key buffer instead of `karac_map_iter_*`.
    /// `val_te` is `Some` for a map and `None` for a set.
    fn emit_sorted_collection_display_fn(
        &mut self,
        type_name: String,
        key_te: &TypeExpr,
        val_te: Option<&TypeExpr>,
        prefix: &str,
    ) -> Result<FunctionValue<'ctx>, String> {
        if let Some(f) = self.disp_cache_get(&type_name) {
            return Ok(f);
        }
        let fn_name = format!("{}{type_name}", self.disp_sym_prefix());
        if let Some(f) = self.module.get_function(&fn_name) {
            self.disp_cache_put(type_name, f);
            return Ok(f);
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
        self.disp_cache_put(type_name, display_fn);

        let entry_bb = self.context.append_basic_block(display_fn, "entry");
        self.builder.position_at_end(entry_bb);
        self.current_fn = Some(display_fn);
        let slot_ptr = display_fn.get_nth_param(0).unwrap().into_pointer_value();
        let acc = display_fn.get_nth_param(1).unwrap().into_pointer_value();

        let body = self
            .emit_sorted_collection_display_body(display_fn, slot_ptr, acc, key_te, val_te, prefix);

        // Restore the caller's position before propagating, so a declined
        // element type leaves the builder where it was found.
        self.current_fn = saved_fn;
        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        body?;

        Ok(display_fn)
    }

    /// Body of a `SortedMap` / `SortedSet` Display fn. Loads the handle,
    /// appends `prefix`, materializes the ascending key buffer via
    /// `emit_sorted_keys_buf`, walks it by index (appending `", "` before
    /// every element but the first), and frees the buffer before appending
    /// `"}"`. For a map each value is fetched with `karac_map_get` against the
    /// same key pointer the render just used, mirroring `compile_for_map_var`'s
    /// sorted arm exactly.
    ///
    /// The key pointer is passed straight to the element Display fn: the
    /// buffer holds raw key bytes (for `String`, the `{ptr, len, cap}` header
    /// aliasing the map's own data), which is the same shape the unsorted
    /// path's `out_key` alloca holds. Nothing in the buffer is owned, so
    /// freeing it after the walk drops no element.
    #[allow(clippy::too_many_arguments)]
    fn emit_sorted_collection_display_body(
        &mut self,
        display_fn: FunctionValue<'ctx>,
        slot_ptr: PointerValue<'ctx>,
        acc: PointerValue<'ctx>,
        key_te: &TypeExpr,
        val_te: Option<&TypeExpr>,
        prefix: &str,
    ) -> Result<(), String> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();
        let key_ty = self.llvm_type_for_type_expr(key_te);

        // Materialize the per-element Display fns first: the recursive emit
        // repositions the builder (it restores, but the sorted-keys call below
        // must land in THIS block, after the handle load).
        let key_disp = self.emit_display_fn_for_type_expr(key_te);
        let val_disp = val_te.map(|v| self.emit_display_fn_for_type_expr(v));
        let out_val = val_te.map(|v| {
            let val_ty = self.llvm_type_for_type_expr(v);
            self.create_entry_alloca(display_fn, "smd.out_val", val_ty)
        });

        self.disp_append_lit(acc, prefix);

        let handle = self
            .builder
            .build_load(ptr_ty, slot_ptr, "smd.handle")
            .unwrap()
            .into_pointer_value();

        let (kbuf, len) = self.emit_sorted_keys_buf(handle, key_te)?;
        let idx_slot = self.create_entry_alloca(display_fn, "smd.i", i64_t.into());
        self.builder
            .build_store(idx_slot, i64_t.const_zero())
            .unwrap();

        let cond_bb = self.context.append_basic_block(display_fn, "smd.cond");
        let body_bb = self.context.append_basic_block(display_fn, "smd.body");
        let sep_bb = self.context.append_basic_block(display_fn, "smd.sep");
        let elem_bb = self.context.append_basic_block(display_fn, "smd.elem");
        let exit_bb = self.context.append_basic_block(display_fn, "smd.exit");
        self.builder.build_unconditional_branch(cond_bb).unwrap();

        self.builder.position_at_end(cond_bb);
        let i = self
            .builder
            .build_load(i64_t, idx_slot, "smd.i.v")
            .unwrap()
            .into_int_value();
        let more = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SLT, i, len, "smd.more")
            .unwrap();
        self.builder
            .build_conditional_branch(more, body_bb, exit_bb)
            .unwrap();

        // A scalar index makes the separator a plain `i != 0` test — no
        // `is_first` alloca is needed (unlike the iterator-driven paths).
        self.builder.position_at_end(body_bb);
        let is_first = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::EQ,
                i,
                i64_t.const_zero(),
                "smd.first",
            )
            .unwrap();
        self.builder
            .build_conditional_branch(is_first, elem_bb, sep_bb)
            .unwrap();

        self.builder.position_at_end(sep_bb);
        self.disp_append_lit(acc, ", ");
        self.builder.build_unconditional_branch(elem_bb).unwrap();

        self.builder.position_at_end(elem_bb);
        let kptr = unsafe {
            self.builder
                .build_gep(key_ty, kbuf, &[i], "smd.kptr")
                .unwrap()
        };
        self.builder
            .build_call(key_disp, &[kptr.into(), acc.into()], "smd.kd")
            .unwrap();
        if let (Some(vd), Some(ov)) = (val_disp, out_val) {
            self.disp_append_lit(acc, ": ");
            self.builder
                .build_call(
                    self.runtime_fns.karac_map_get_fn,
                    &[handle.into(), kptr.into(), ov.into()],
                    "smd.get",
                )
                .unwrap();
            self.builder
                .build_call(vd, &[ov.into(), acc.into()], "smd.vd")
                .unwrap();
        }
        let inc = self
            .builder
            .build_int_add(i, i64_t.const_int(1, false), "smd.inc")
            .unwrap();
        self.builder.build_store(idx_slot, inc).unwrap();
        self.builder.build_unconditional_branch(cond_bb).unwrap();

        self.builder.position_at_end(exit_bb);
        self.builder
            .build_call(self.runtime_fns.free_fn, &[kbuf.into()], "")
            .unwrap();
        self.disp_append_lit(acc, "}");
        self.builder.build_return(None).unwrap();
        Ok(())
    }

    /// Deeply mangled type name suitable for Display cache keys. Unlike
    /// `mangled_type_name` (which is shallow on `Path` types — used for
    /// hash/eq, where `Map[Vec[T], V]` is unreachable so deep mangling is
    /// unnecessary), this walks generic args so `Vec[i64]` → `Vec_i64`,
    /// `Map[String, i64]` → `Map_String_i64`, and nested shapes compose.
    /// Tuples use the same `tuple_T1_T2_...` form `mangled_type_name`
    /// produces — the recursive shapes match.
    /// B-2026-07-08-9: true iff `te` is a payload type the Option/Result Display
    /// synthesizer can correctly render — a primitive or String that fits inline
    /// in the enum's payload words and round-trips through
    /// `rebuild_value_from_payload_words` (the 3-word reconstruction
    /// `emit_enum_field_display` uses). Compound payloads (structs, tuples,
    /// nested Option/Result, collections, boxed/wide types) return false so the
    /// display path falls through to the generic error rather than emit invalid
    /// IR from a mis-sized reconstruction (`Option[Wide]` boxes its payload as a
    /// pointer, which the inline path would mis-read).
    pub(super) fn is_inline_displayable_payload(te: &TypeExpr) -> bool {
        if let TypeKind::Path(p) = &te.kind {
            if let Some(seg) = p.segments.last() {
                // The 128-bit widths are inline-displayable like any other
                // scalar — they occupy TWO payload words rather than one,
                // which the word rejoin in `rebuild_value_from_payload_words`
                // handles. Absent here, `println(o)` on an `Option[i128]` fell
                // through to the struct-argument error path and reported "bind
                // a struct literal to a `let` first" about a plain variable
                // (B-2026-08-19-23).
                //
                // B-2026-08-31-10: this was a hand-written width list that
                // stopped at f32/f64, so `Option[f16]` / `Option[bf16]` hit
                // that same misleading error even though
                // `rebuild_value_from_payload_words`'s float branch has
                // truncated-then-bitcast narrow floats correctly since
                // B-2026-07-20-12. `PRELUDE_PRIMITIVES` is the canonical list
                // every other module reads; `str` is the one extra name here
                // (it is not a prelude primitive but shares String's layout).
                return crate::prelude::PRELUDE_PRIMITIVES.contains(&seg.as_str())
                    || seg.as_str() == "str";
            }
        }
        false
    }

    /// A field type that occupies exactly ONE payload word, so a struct made of
    /// them reconstructs from the ≤3 sequential inline-payload words. Scalars
    /// only — String/str are 3-word `{ptr,len,cap}` values and would overflow.
    pub(super) fn is_scalar_word_display_field(te: &TypeExpr) -> bool {
        if let TypeKind::Path(p) = &te.kind {
            if let Some(seg) = p.segments.last() {
                return matches!(
                    seg.as_str(),
                    "i8" | "i16"
                        | "i32"
                        | "i64"
                        | "isize"
                        | "u8"
                        | "u16"
                        | "u32"
                        | "u64"
                        | "usize"
                        | "f32"
                        | "f64"
                        | "bool"
                        | "char"
                );
            }
        }
        false
    }

    /// Payload types whose Display codegen can reconstruct from the ≤3-word
    /// inline enum-payload area and render: the `is_inline_displayable_payload`
    /// set (primitives + String), PLUS a small user struct whose fields are all
    /// SCALAR (one word each) and number ≤ 3 — `rebuild_value_from_payload_words`
    /// reconstructs it field-by-field and `emit_struct_debug_display_fn` renders
    /// it in debug format (B-2026-07-08-18, e.g. `Option[Point]`). A struct with
    /// a String field (3 words) or > 3 fields overflows the inline area (the
    /// payload is heap-BOXED), so it stays on the deferred path (a clean error,
    /// not invalid IR).
    /// Peel a single `ref` / `mut ref` layer from an Option/Result Display
    /// payload `TypeExpr` when the borrowed inner is a SCALAR primitive.
    ///
    /// `Vec.first()` / `.get(i)` / `.last()` are typed `Option[ref T]` — the
    /// borrow-typed accessor (B-2026-07-14-11). For a scalar element the borrow
    /// is a pure type-system artifact: the accessor returns the value BY COPY in
    /// the Some payload word, so the runtime layout is byte-identical to a plain
    /// `Option[T]` (proven by the annotated `let x: Option[i64] = v.first()` form
    /// already rendering the correct value). Peeling lets the Display registration
    /// (`var_option_payload_te` / `var_result_payload_te`) recognise the inline
    /// payload for an UNannotated `let x = v.first()`; without it the whole
    /// binding falls through to the deferred struct-Display error (a run-vs-build
    /// divergence — the interpreter renders it, codegen refused).
    ///
    /// Peels a scalar inner as above, AND a `ref String` / `ref str`: the
    /// borrow-typed `Vec[String].get(i)` / `.first()` / `.last()` return
    /// `Option[ref String]`, but codegen builds the `Some` payload by loading the
    /// element's whole `{ptr,len,cap}` and coercing it to the 3 inline payload
    /// words (`vec_method.rs` `get`/`first`/`last`, `coerce_to_payload_words(_, 3)`)
    /// — byte-identical to a plain `Option[String]`. So the DISPLAY render is the
    /// same; peeling routes it through the owned-`String` renderer (B-2026-07-18-39
    /// sibling). Display is read-only and never frees the payload buffer (the
    /// renderer appends a COPY of the bytes into a fresh accumulator), so the
    /// borrow's shared Vec buffer is untouched — no double-free. A `ref Vec` /
    /// other compound borrow still stays on the deferred path.
    pub(super) fn peel_scalar_ref_display_payload(te: &TypeExpr) -> TypeExpr {
        if let TypeKind::Ref(inner) | TypeKind::MutRef(inner) = &te.kind {
            if Self::is_scalar_word_display_field(inner)
                || Self::is_inline_displayable_payload(inner)
            {
                return (**inner).clone();
            }
        }
        te.clone()
    }

    pub(super) fn is_reconstructable_display_payload(&self, te: &TypeExpr) -> bool {
        self.reconstructable_display_payload_inner(te, &mut Vec::new())
    }

    /// `seen` is the stack of user-struct names currently being expanded. A
    /// RECURSIVE type re-enters its own name and would otherwise recurse
    /// forever — `shared struct ListNode { val: i64, next: Option[ListNode] }`
    /// overflowed the compiler's stack rather than answering, on a program that
    /// already compiled and rendered before this predicate learned to walk
    /// struct fields at all (B-2026-08-31-10). A repeat is answered `true`: the
    /// only way to build a finite recursive type is through an indirection
    /// (`shared`, or an `Option` that boxes), so the cycle's own edge always
    /// reconstructs — and the surrounding fields are still checked on the way
    /// down.
    fn reconstructable_display_payload_inner(&self, te: &TypeExpr, seen: &mut Vec<String>) -> bool {
        if Self::is_inline_displayable_payload(te) {
            return true;
        }
        match &te.kind {
            // A tuple is rebuilt FIELD BY FIELD from sequential words
            // (`rebuild_value_from_payload_word_slice`'s struct branch), so the
            // recursion is the honest condition: every field must reconstruct.
            TypeKind::Tuple(elems) => {
                !elems.is_empty()
                    && elems
                        .iter()
                        .all(|e| self.reconstructable_display_payload_inner(e, seen))
            }
            TypeKind::Array { element, size } => {
                matches!(&size.kind, ExprKind::Integer(v, _) if *v >= 0)
                    && self.reconstructable_display_payload_inner(element, seen)
            }
            TypeKind::Path(p) => {
                let Some(seg) = p.segments.last().map(String::as_str) else {
                    return false;
                };
                // HANDLE-shaped stdlib collections. The payload is the
                // `{ptr, len, cap}` triple (or a bare handle pointer) that the
                // reconstruction already round-trips — the SAME shape as
                // `String`, which this predicate has always admitted. Element
                // types take no part in the reconstruction; they are only
                // rendered, by the type-directed dispatcher. `VecDeque` is
                // absent because it has no `Display` at all — the typechecker
                // rejects the interpolation before codegen sees it — and this
                // list holds only shapes that were measured against the
                // interpreter.
                if matches!(seg, "Vec" | "Map" | "Set" | "SortedMap" | "SortedSet") {
                    return true;
                }
                // A `shared` type is an RC HANDLE — one pointer word, whatever
                // its fields are — so it reconstructs by `inttoptr` and its
                // renderer walks the pointee. Checked ahead of the by-value
                // struct arm below for the same reason
                // `emit_display_fn_for_type_expr` checks `shared_types` ahead of
                // `struct_field_names`: a shared type is registered in BOTH, and
                // the field walk is wrong for it.
                if self.type_decls.shared_types.contains_key(seg) {
                    return true;
                }
                // A NESTED Option/Result is a plain struct of words, and the
                // deboxing unpack handles the spilled case.
                if matches!(seg, "Option" | "Result") {
                    return p.generic_args.as_ref().is_some_and(|args| {
                        args.iter().all(|a| match a {
                            GenericArg::Type(t) => {
                                self.reconstructable_display_payload_inner(t, seen)
                            }
                            _ => false,
                        })
                    });
                }
                // `Vector[T, N]` and `Array[T, N]` were EXCLUDED here when
                // B-2026-08-31-10 widened this predicate, and the exclusion is
                // worth remembering rather than just deleting: admitting them
                // then printed `Some(Vector(0, 94011124162560, 1, 0))` and
                // `Some(1)` against the interpreter, because the enum-payload
                // word accounting sized both at ONE word (B-2026-08-31-18) and
                // the Display dispatcher had no array arm at all
                // (B-2026-08-31-19). With both fixed they reconstruct and
                // render, and the two ROWS ARE THE ONLY REASON THIS WAS EVER
                // narrower — so anything that reopens either has to put the
                // exclusion back, not paper over it here.
                if matches!(seg, "Vector" | "Array") {
                    return true;
                }
                // `Slice[T]` stays out: it has no Display under codegen at ANY
                // depth — a bare `f"{s}"` on a slice is a hard error too, so
                // there is nothing to reconstruct INTO yet.
                if matches!(seg, "Slice") {
                    return false;
                }
                // A user struct: rebuilt field by field, so recurse. The old
                // rule here was "≤ 3 fields, all one-word scalars", which
                // refused a struct with a `String` field or a fourth `i64`
                // even though `rebuild_payload_deboxing_if_spilled` handles
                // both (measured: `Option[P]` with `P { x: i64, y: String }`
                // and `Option[Q]` with four `i64`s both render identically to
                // the interpreter).
                if let Some(field_tes) = self.type_decls.struct_field_type_exprs.get(seg) {
                    if seen.iter().any(|s| s == seg) {
                        return true;
                    }
                    seen.push(seg.to_string());
                    let ok = !field_tes.is_empty()
                        && field_tes
                            .iter()
                            .all(|f| self.reconstructable_display_payload_inner(f, seen));
                    seen.pop();
                    return ok;
                }
                // A user enum reaching here already has a `Display` — the
                // typechecker rejects the f-string otherwise — and its own
                // renderer walks its variants.
                self.type_decls.enum_layouts.contains_key(seg)
            }
            _ => false,
        }
    }

    /// B-2026-08-31-10 — the message for a Display the synthesizer declines.
    ///
    /// One string used to serve two very different arrivals here. The first is
    /// the one it describes: an f-string (or `println`) interpolating a struct
    /// LITERAL or a call result, where binding to a `let` really is the fix.
    /// The second is an `Option`/`Result` PLACE EXPRESSION whose payload shape
    /// `is_reconstructable_display_payload` declines — already a `let`-bound
    /// variable, so the prescribed remedy was not merely unhelpful but false,
    /// and the text named neither the type nor the part of it that is
    /// unsupported. `f"{o}"` on an `Option[Vector[i64, 4]]` read as advice to
    /// bind a variable that is three lines up.
    ///
    /// `arg` distinguishes the `println(x)` spelling from the f-string one so
    /// each keeps its own example, which is the only thing the two messages
    /// ever differed in.
    ///
    /// The declined-payload arm deliberately prescribes NO source-level
    /// rewrite. The obvious one — destructure with `match`/`if let` and format
    /// the binding — does not help for the shape that still reaches it: a
    /// `Slice[T]` payload bound by a match arm hits the same refusal, because
    /// codegen has no slice Display at any depth. When `Vector`/`Array` were
    /// the declined shapes (before B-2026-08-31-18 and -19) the advice was
    /// worse than useless — destructuring them bound `Some(0)` / `Some(1)`
    /// against the interpreter's real values — so it would have traded a clean
    /// compile error for silently wrong output. The one honest escape is the
    /// backend that does render it.
    pub(super) fn deferred_display_error(&self, e: &Expr, arg: bool) -> String {
        let key = (e.span.offset, e.span.length);
        if let Some(te) = self.display.display_option_result_types.get(&key) {
            let full = crate::formatter::render_type_expr(te);
            let payloads: Vec<TypeExpr> = if let Some(pte) = Self::option_payload_te(te) {
                vec![pte]
            } else if let Some((ok_te, err_te)) = Self::result_payload_tes(te) {
                vec![ok_te, err_te]
            } else {
                Vec::new()
            };
            let declined = payloads.into_iter().find(|pte| {
                !self
                    .is_reconstructable_display_payload(&Self::peel_scalar_ref_display_payload(pte))
            });
            if let Some(pte) = declined {
                let payload = crate::formatter::render_type_expr(&pte);
                return format!(
                    "Display of `{full}` is not yet supported under codegen: its `{payload}` \
                     payload cannot be reconstructed from the enum's inline payload words. \
                     The tree-walk backend (`karac run --interp`) renders it."
                );
            }
            return format!("Display of `{full}` is not yet supported under codegen.");
        }
        // A place expression that got here is NOT the literal/call-result case
        // the advice below addresses, so do not offer it.
        if matches!(
            e.kind,
            ExprKind::Identifier(_) | ExprKind::FieldAccess { .. }
        ) {
            return "Display of this value is not yet supported under codegen (user-struct \
                    Display, subtask-5 follow-on)"
                .to_string();
        }
        if arg {
            "Display of a struct argument is supported when the argument is a variable or \
             field access (e.g. `let x = …; println(x)` / `x.to_string()`); bind a struct \
             literal or call result to a `let` first (user-struct Display, subtask-5 \
             follow-on)"
                .to_string()
        } else {
            "Display of a struct in an f-string is supported when the interpolated expression \
             is a variable or field access (e.g. `f\"{x}\"`); bind a struct literal or call \
             result to a `let` first (user-struct Display, subtask-5 follow-on)"
                .to_string()
        }
    }

    /// Mangle a `TypeExpr` into the identifier fragment that keys every
    /// synthesized per-type function (display, clone, drop, eq, hash).
    ///
    /// CONST generic args are part of the mangle, not decoration: a type whose
    /// identity includes an extent — `Array[T, N]`, `Vector[T, N]` — has one
    /// distinct layout per N, so dropping N collapses `Array[i64, 2]` and
    /// `Array[i64, 3]` onto one name and the first-emitted body silently
    /// serves both (B-2026-08-27-25). The same reason `TypeKind::Array` gets
    /// its own arm rather than falling to `"unknown"`, which collapses it onto
    /// every other unhandled shape.
    pub(super) fn display_mangle_te(te: &TypeExpr) -> String {
        match &te.kind {
            TypeKind::Tuple(elems) if elems.is_empty() => "unit".to_string(),
            TypeKind::Tuple(elems) => {
                let parts: Vec<String> = elems.iter().map(Self::display_mangle_te).collect();
                format!("tuple_{}", parts.join("_"))
            }
            // Inference-recovered form of `Array[T, N]` (`type_to_type_expr`);
            // the source-written form is a `Path` and takes the arm below.
            TypeKind::Array { element, size } => {
                let elem = Self::display_mangle_te(element);
                match &size.kind {
                    ExprKind::Integer(n, _) => format!("Array_{elem}_{n}"),
                    _ => format!("Array_{elem}"),
                }
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
                            GenericArg::Const(e) => Some(match &e.kind {
                                ExprKind::Integer(n, _) => n.to_string(),
                                ExprKind::Identifier(name) => name.clone(),
                                _ => "const".to_string(),
                            }),
                            GenericArg::Shape(_) => None,
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
        if let Some(f) = self.disp_cache_get(&type_name) {
            return f;
        }
        let fn_name = format!("{}{type_name}", self.disp_sym_prefix());
        if let Some(f) = self.module.get_function(&fn_name) {
            self.disp_cache_put(type_name, f);
            return f;
        }

        match &te.kind {
            TypeKind::Tuple(elems) if !elems.is_empty() => self.emit_tuple_display_fn(elems),
            // B-2026-08-31-19 — `Array[T, N]`, the shape this dispatcher never
            // grew an arm for. Every NESTED spelling of an array (a `Vec`
            // element, a struct field, a tuple field, an enum payload, a `dbg`
            // argument) fell through to the by-name catch-all below and
            // PANICKED the compiler with `type_name 'Array_i64_3' not yet
            // supported`. Sits with the other extent-bearing shapes because the
            // size is part of the type's identity, not decoration.
            TypeKind::Array { element, size } => {
                let n = match &size.kind {
                    ExprKind::Integer(v, _) if *v >= 0 => *v as u64,
                    _ => 0,
                };
                let elem = (**element).clone();
                self.emit_array_display_fn(&elem, n, te)
            }
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
                // B-2026-08-30-39 — `Vector[T, N]`. Without this arm every
                // NESTED spelling of a vector (an element of a `Vec`, a tuple
                // field, any `dbg` argument) fell through to the by-name
                // catch-all below and panicked the compiler, while the same
                // value at depth 0 rendered correctly through the f-string part
                // renderer. Sits with the other extent-bearing shapes because
                // the const arg is part of the identity, not decoration.
                if head == Some("Vector") {
                    let args = p.generic_args.as_ref();
                    let elem_te = args.and_then(|a| a.first()).and_then(|a| match a {
                        GenericArg::Type(t) => Some(t.clone()),
                        _ => None,
                    });
                    let lanes = args.and_then(|a| a.get(1)).and_then(|a| match a {
                        GenericArg::Const(e) => match &e.kind {
                            ExprKind::Integer(n, _) => Some(*n),
                            _ => None,
                        },
                        _ => None,
                    });
                    if let (Some(elem), Some(n)) = (elem_te, lanes) {
                        return self.emit_vector_display_fn(&elem, n, te);
                    }
                }
                if head == Some("Set") {
                    if let Some(GenericArg::Type(elem_te)) =
                        p.generic_args.as_ref().and_then(|a| a.first()).cloned()
                    {
                        return self.emit_set_display_fn(&elem_te);
                    }
                }
                // `Array[T, N]` has TWO surface spellings and both reach here:
                // source-written `Array[i64, 3]` parses as this PATH form,
                // while a `TypeExpr` synthesized from a `Type::Array` (the
                // typechecker's `type_to_type_expr`) uses `TypeKind::Array`,
                // handled by the arm at the top of this match. A fix that
                // covered only one left the other panicking — the path form is
                // what a `Vec[Array[i64, 3]]` element carries
                // (B-2026-08-31-19).
                if head == Some("Array") {
                    let args = p.generic_args.as_ref();
                    let elem_te = args.and_then(|a| a.first()).and_then(|a| match a {
                        GenericArg::Type(t) => Some(t.clone()),
                        _ => None,
                    });
                    let n = args.and_then(|a| a.get(1)).and_then(|a| match a {
                        GenericArg::Const(e) => match &e.kind {
                            ExprKind::Integer(v, _) if *v >= 0 => Some(*v as u64),
                            _ => None,
                        },
                        _ => None,
                    });
                    if let (Some(elem), Some(n)) = (elem_te, n) {
                        return self.emit_array_display_fn(&elem, n, te);
                    }
                }
                // B-2026-08-14-35 — the sorted siblings. Absent these arms a
                // NESTED sorted collection (`Vec[SortedMap[K, V]]`, a struct
                // field of that shape) fell all the way through to the by-name
                // catch-all and panicked the compiler with
                // `type_name 'SortedMap_String_i64' not yet supported`; the
                // non-nested spellings reached the unsorted renderers instead
                // and printed the wrong prefix in bucket order.
                //
                // This dispatcher is not `Result`-returning and is recursive
                // through every element/field shape, so a declined comparator
                // surfaces as a panic here — the same convention every other
                // unsupported arm in this file already uses, and with a message
                // that names the escape hatch instead of a mangled cache key.
                // The DIRECT display entry points propagate the error cleanly
                // instead, which is where any reachable case lands: the
                // typechecker already restricts `SortedMap`/`SortedSet` keys to
                // `Ord` types and further requires the key to be `Display` here,
                // and every such type codegen's comparator supports.
                if head == Some("SortedMap") {
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
                        return self
                            .emit_sorted_map_display_fn(&k, &v)
                            .unwrap_or_else(|e| panic!("emit_display_fn_for_type_expr: {e}"));
                    }
                }
                if head == Some("SortedSet") {
                    if let Some(GenericArg::Type(elem_te)) =
                        p.generic_args.as_ref().and_then(|a| a.first()).cloned()
                    {
                        return self
                            .emit_sorted_set_display_fn(&elem_te)
                            .unwrap_or_else(|e| panic!("emit_display_fn_for_type_expr: {e}"));
                    }
                }
                // User enum (possibly payload-bearing) — value-driven Display
                // fn (the all-unit `compile_unit_enum_display` is select-chain
                // and expr-driven; this path is the buffer-append, by-pointer
                // form needed for nested/recursive field rendering).
                if let Some(seg) = p.segments.last() {
                    // A hand-written `impl Display` wins over every built-in
                    // renderer below, at EVERY depth (B-2026-08-26-29). The
                    // expression-driven sites (`println(x)` / `f"{x}"` /
                    // `x.to_string()`) already prefer the user method via
                    // `user_display_impl_type`, but this by-pointer path is the
                    // one every CONTAINER element goes through — so before this,
                    // `f"{e}"` printed the user's text while `f"{[e]}"` printed
                    // the derived shape one level down.
                    //
                    // Checked ahead of the shared / enum / struct arms for the
                    // same reason the shared arm precedes the other two: those
                    // claim the type by name and would emit the built-in shape
                    // without ever asking whether the user wrote an impl.
                    //
                    // `debug_render` gates it, exactly as the interpreter's
                    // walker takes this branch only when `debug` is false:
                    // `Debug` is a DIFFERENT trait and keeps the field-name
                    // shape. This dispatcher serves both modes (the two
                    // families differ only by `disp_sym_prefix` / the mode's
                    // own cache), so without the gate a user `impl Display`
                    // would capture `dbg()` too — `dbg(e)` on an `AllocError`
                    // reported its Display prose instead of
                    // `OutOfMemory { requested_bytes: 4096 }`.
                    if !self.display.debug_render && self.user_display_impl_fn(seg).is_some() {
                        return self.emit_user_display_impl_fn(seg);
                    }
                    // B-2026-08-24-2 — a `shared struct` handle. Checked BEFORE
                    // the enum and struct arms below, because a shared type is
                    // registered in BOTH `struct_field_names` (which the struct
                    // arm claims by name) and, for a shared enum, `enum_layouts`
                    // — while its LLVM type lives in `shared_types` and NOT in
                    // `struct_types`. That mismatch is what made the struct
                    // renderer's `.expect("struct type registered")` panic the
                    // compiler on a shared value, which `dbg`'s pre-check now
                    // refuses ahead of. A shared ENUM still falls through to the
                    // refusal: its payload lives in the RC box behind a variant
                    // tag and needs its own renderer, so it keeps failing closed
                    // rather than rendering something the interpreter disagrees
                    // with.
                    if let Some(info) = self.type_decls.shared_types.get(seg) {
                        if info.is_enum {
                            return self.emit_shared_enum_debug_display_fn(seg);
                        }
                        return self.emit_shared_struct_debug_display_fn(seg);
                    }
                    if self.type_decls.enum_layouts.contains_key(seg) {
                        // Pass the use site's generic arguments so a generic
                        // enum renders at its instantiation (B-2026-08-19-28).
                        let args: Vec<GenericArg> = p.generic_args.clone().unwrap_or_default();
                        return self.emit_enum_display_fn(seg, &args);
                    }
                    // Total-order float wrappers render as their INNER float
                    // (B-2026-08-11-8), so they must be pulled out before the
                    // struct check below. They are real entries in
                    // `struct_field_names` (seeded in `declarations.rs`), so
                    // that check would otherwise claim them and emit the
                    // struct-debug shape — `F64 { value: 3.14 }` where the
                    // interpreter prints `3.14`, a run-vs-build divergence on
                    // every `println` of a wrapper. Covers the nested cases
                    // too: `Vec[F64]` recurses here for its element.
                    if self.is_prelude_total_float_wrapper(seg.as_str()) {
                        let llvm_ty = self.llvm_type_for_type_expr(te);
                        return self.emit_display_fn_for_type(seg, llvm_ty);
                    }
                    // User struct nested in another type's Display (an enum
                    // payload / collection element): debug/field format,
                    // matching the interpreter (B-2026-07-08-18). Without this a
                    // struct-typed field fell through to `emit_display_fn_for_type`
                    // and panicked ("type_name … not yet supported").
                    if self.type_decls.struct_field_names.contains_key(seg) {
                        return self.emit_struct_debug_display_fn(seg);
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
        if let Some(f) = self.disp_cache_get(&type_name) {
            return f;
        }
        let fn_name = format!("{}{type_name}", self.disp_sym_prefix());
        if let Some(f) = self.module.get_function(&fn_name) {
            self.disp_cache_put(type_name, f);
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
        self.disp_cache_put(type_name, display_fn);

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
        let fn_name = format!("{}{type_name}", self.disp_sym_prefix());
        if let Some(f) = self.disp_cache_get(&type_name) {
            return f;
        }
        if let Some(f) = self.module.get_function(&fn_name) {
            self.disp_cache_put(type_name, f);
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
        self.disp_cache_put(type_name, display_fn);

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
                if self.type_decls.struct_field_names.contains_key(seg) {
                    return Some(seg.clone());
                }
            }
        }
        None
    }

    /// True when `te` denotes the `std.secret` `Secret[T]` wrapper (a path
    /// whose last segment is `Secret`) and that stdlib type is in scope for
    /// this compilation. Drives `<redacted>` emission in the derived-Display
    /// field walk. Scoped via `secret_type_is_stdlib` so a user's own
    /// `struct Secret` is unaffected.
    fn field_type_is_stdlib_secret(&self, te: &crate::ast::TypeExpr) -> bool {
        self.contract_state.secret_type_is_stdlib
            && matches!(
                &te.kind,
                crate::ast::TypeKind::Path(p)
                    if p.segments.last().map(|s| s.as_str()) == Some("Secret")
            )
    }

    /// True when `te` is a leaf the f-string lowering can format directly: a
    /// primitive / String, or an all-unit enum (whose interpolation part is
    /// handled by `fstr_render_part` via `compile_unit_enum_display`).
    fn display_field_is_leaf(&self, te: &crate::ast::TypeExpr) -> bool {
        if let crate::ast::TypeKind::Path(p) = &te.kind {
            if let Some(seg) = p.segments.last() {
                // B-2026-08-31-9 — the canonical primitive list first, so this
                // copy cannot drift again. It omitted `f16` and `bf16`, so a
                // `#[derive(Display)]` struct with a narrow-float FIELD refused
                // to compile, while the same field read on its own (`f"{h.a}"`)
                // rendered correctly on both backends — the refusal inverted
                // with respect to difficulty, exactly as the B-2026-08-26-33
                // note below records for the enum case.
                //
                // `Vector[T, N]` is deliberately NOT admitted here — see
                // `struct_display_needs_type_directed_fn`.
                if crate::prelude::PRELUDE_PRIMITIVES.contains(&seg.as_str()) {
                    return true;
                }
                return matches!(
                    seg.as_str(),
                    "i8" | "i16"
                        | "i32"
                        | "i64"
                        // B-2026-08-19-23 widened three sibling lists to the
                        // 128-bit widths and missed this one, so a `u128` field
                        // in a `#[derive(Display)]` struct still refused to
                        // compile ("whose Display is not yet supported"). The
                        // leaf path behind it routes through
                        // `emit_display_fn_for_type`, which that row DID give a
                        // 128-bit arm, so the names are all that was missing.
                        | "i128"
                        | "u8"
                        | "u16"
                        | "u32"
                        | "u64"
                        | "u128"
                        | "usize"
                        | "isize"
                        | "f32"
                        | "f64"
                        | "bool"
                        | "char"
                        | "String"
                ) || self
                    .type_decls
                    .enum_unit_variants
                    .contains_key(seg.as_str())
                    // Any enum, not just an all-unit one (B-2026-08-26-33).
                    // The leaf branch emits `P::Expr(field_expr)`, i.e. exactly
                    // the `f"{h.u}"` spelling — and that spelling ALREADY
                    // renders a payload-bearing enum, a tuple-variant enum, a
                    // generic enum and an `Option` field identically on both
                    // backends. Only this predicate refused them, which is why
                    // the refusal was inverted with respect to difficulty: the
                    // same struct rendered fine NESTED in a `Vec` (that path is
                    // `emit_struct_debug_display_fn`, which walks fields through
                    // `emit_display_fn_for_type_expr` and has no such list) and
                    // the field rendered fine on its own, while interpolating
                    // the struct itself failed the whole compile.
                    //
                    // NON-GENERIC only, and that restriction is load-bearing
                    // rather than cautious. The leaf branch synthesizes its
                    // field expression with the BASE's span, so nothing
                    // downstream can recover the field's own instantiation —
                    // the nested renderer threads use-site generic args
                    // explicitly (B-2026-08-19-28) and this path has no
                    // equivalent. Admitting a generic enum therefore does not
                    // merely fail, it PANICS the compiler in
                    // `emit_display_fn_for_type` ("type_name 'T' not yet
                    // supported"), and admitting `Option[i64]` — seeded in
                    // `enum_layouts`, so it would otherwise qualify — trades
                    // this function's accurate "field 'u' has a type … not yet
                    // supported" for the f-string path's misleading advice to
                    // bind a struct literal to a `let`. Both stay on the clean
                    // error until the leaf path can carry an instantiation.
                    //
                    // This list has been the narrow half before: B-2026-08-19-23
                    // widened three sibling lists to the 128-bit widths and
                    // missed this one, leaving a `u128` field refused after the
                    // feature had otherwise landed. Same shape, same cause — a
                    // capability exists one layer down and the leaf list does
                    // not know about it.
                    || (p.generic_args.as_ref().is_none_or(|a| a.is_empty())
                        && self.type_decls.enum_layouts.contains_key(seg.as_str()));
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
            .type_decls
            .struct_field_names
            .get(type_name)
            .cloned()
            .ok_or_else(|| format!("Display: unknown struct '{type_name}'"))?;
        let field_tes = self
            .type_decls
            .struct_field_type_exprs
            .get(type_name)
            .cloned()
            .unwrap_or_default();
        // Total-order float wrappers (B-2026-08-11-8) render as the bare
        // inner float — `3.14`, not `F64 { value: 3.14 }`. Emitting just the
        // `.value` part reuses the ordinary float interpolation path, so the
        // wrapper prints byte-identically to the primitive it wraps and to
        // the interpreter's `Display for Value`. This is the single seam that
        // covers BOTH `println(x)` and `f"{x}"`: they both arrive here via
        // `expr_user_struct_name` → `compile_struct_display_string`.
        if self.is_prelude_total_float_wrapper(type_name) {
            return Ok(vec![P::Expr(
                Box::new(Expr {
                    kind: ExprKind::FieldAccess {
                        object: Box::new(base.clone()),
                        field: "value".to_string(),
                    },
                    span: base.span,
                }),
                None,
            )]);
        }
        let mut parts: Vec<P> = vec![P::Text(format!("{type_name} {{ "))];
        for (i, fname) in field_names.iter().enumerate() {
            if i > 0 {
                parts.push(P::Text(", ".to_string()));
            }
            parts.push(P::Text(format!("{fname}: ")));
            let te = field_tes.get(i);
            // std.secret: a `Secret[T]` field never renders its wrapped value
            // in a derived Debug/Display — emit the literal `<redacted>`. This
            // sits ahead of the nested-struct dispatch below, which would
            // otherwise recurse into `Secret { inner: <value> }` and leak it.
            // Nesting through non-`Secret` structs is covered automatically:
            // the recursion into each nested struct hits this same check for
            // its own `Secret` fields.
            if te
                .map(|t| self.field_type_is_stdlib_secret(t))
                .unwrap_or(false)
            {
                parts.push(P::Text("<redacted>".to_string()));
                continue;
            }
            let field_expr = Expr {
                kind: ExprKind::FieldAccess {
                    object: Box::new(base.clone()),
                    field: fname.clone(),
                },
                span: base.span,
            };
            match te.and_then(|t| self.display_field_struct_name(t)) {
                Some(nested) => {
                    parts.extend(self.build_struct_display_parts(&field_expr, &nested)?);
                }
                None => {
                    if te.map(|t| self.display_field_is_leaf(t)).unwrap_or(false) {
                        parts.push(P::Expr(Box::new(field_expr), None));
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
    /// B-2026-08-31-9 — does this struct have a field the F-STRING PARTS path
    /// cannot render but the TYPE-DIRECTED one can?
    ///
    /// Today that is exactly `Vector[T, N]`, and the reason is the parts path's
    /// one structural limitation: it synthesizes each field's expression with
    /// the BASE's span, and a vector interpolation resolves through the
    /// span-keyed `vector_typed_exprs` table — so the synthesized part looks up
    /// a span belonging to the struct, misses, and falls through to the SCALAR
    /// renderer. Measured: admitting `Vector` as a leaf makes `f"{h}"` print
    /// `Holder { v: 1, n: 1 }` — one stray lane, the exact B-2026-08-29-52
    /// defect — while `dbg(h)` on the same value is correct.
    ///
    /// The type-directed renderer has no such dependency: it dispatches on the
    /// field's `TypeExpr` and reaches the `Vector` arm B-2026-08-30-39 added.
    /// It is also already proven on this shape — the SAME struct nested in a
    /// `Vec` renders correctly today, because a container element goes through
    /// `emit_struct_debug_display_fn`. This routes the top-level spelling to
    /// the path the nested one already takes.
    ///
    /// Narrow on purpose: a positive test for the one field class known to need
    /// it, rather than a catch-all fallback on `build_struct_display_parts`
    /// failing. A catch-all would also swallow genuinely unsupported shapes,
    /// and `emit_display_fn_for_type_expr` PANICS rather than erroring for
    /// those — trading a clean diagnostic for a compiler crash.
    fn struct_display_needs_type_directed_fn(&self, type_name: &str) -> bool {
        self.type_decls
            .struct_field_type_exprs
            .get(type_name)
            .is_some_and(|tes| {
                tes.iter().any(|te| {
                    // `Array[T, N]` joins `Vector` here for the same reason and
                    // in BOTH its surface spellings — the path form and the
                    // `[T; N]` form (B-2026-08-31-19). Without it, the
                    // field-parts path refused the struct outright with a Rust
                    // `{:?}` of the field's `TypeExpr`, while the same struct
                    // nested one level down rendered fine.
                    matches!(&te.kind, crate::ast::TypeKind::Array { .. })
                        || matches!(&te.kind, crate::ast::TypeKind::Path(p)
                        if matches!(
                            p.segments.last().map(|s| s.as_str()),
                            Some("Vector") | Some("Array")
                        ))
                })
            })
    }

    pub(super) fn compile_struct_display_string(
        &mut self,
        base: &Expr,
        type_name: &str,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        // B-2026-08-31-9 — route a struct with a `Vector` field through the
        // by-pointer renderer the nested spelling already uses.
        if self.struct_display_needs_type_directed_fn(type_name) {
            let val = self.compile_expr(base)?;
            let fn_val = self
                .current_fn
                .ok_or_else(|| "struct Display: no current function".to_string())?;
            let slot = self.create_entry_alloca(fn_val, "sd.val", val.get_type());
            self.builder.build_store(slot, val).unwrap();
            let disp = self.emit_struct_debug_display_fn(type_name);
            let (_acc, out) = self.render_via_display_fn(disp, slot);
            return Ok(out);
        }
        let parts = self.build_struct_display_parts(base, type_name)?;
        let lit = Expr {
            kind: ExprKind::InterpolatedStringLit(parts),
            span: base.span,
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

    /// The compiled `<type_name>.to_string` function, when the program carries
    /// a hand-written `impl Display` for that type. The function NAME is the
    /// discriminator: only a user impl produces one: the built-in and derived
    /// renderers are named `karac_display_<T>`. Value-driven twin of
    /// `user_display_impl_type`, which asks the same question of an `Expr`.
    pub(super) fn user_display_impl_fn(&self, type_name: &str) -> Option<FunctionValue<'ctx>> {
        self.module.get_function(&format!("{type_name}.to_string"))
    }

    /// Emit (or reuse) the append-form Display fn for a type with a
    /// hand-written `impl Display`: call the user's `to_string` and append the
    /// String it returns to the accumulator.
    ///
    /// Signature matches every other `karac_display_<T>`
    /// (`void(ptr value, ptr acc)`), which is what lets a container element,
    /// a struct field, and an enum payload all reach a user impl through the
    /// one dispatcher they already call (B-2026-08-26-29).
    pub(super) fn emit_user_display_impl_fn(&mut self, type_name: &str) -> FunctionValue<'ctx> {
        let key = type_name.to_string();
        if let Some(f) = self.disp_cache_get(&key) {
            return f;
        }
        let fn_name = format!("{}{key}", self.disp_sym_prefix());
        if let Some(f) = self.module.get_function(&fn_name) {
            self.disp_cache_put(key, f);
            return f;
        }
        let user_fn = self
            .user_display_impl_fn(type_name)
            .expect("emit_user_display_impl_fn: caller checked the user impl exists");

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
        self.disp_cache_put(key, display_fn);

        let entry_bb = self.context.append_basic_block(display_fn, "entry");
        self.builder.position_at_end(entry_bb);
        self.current_fn = Some(display_fn);
        let val_ptr = display_fn.get_nth_param(0).unwrap().into_pointer_value();
        let acc = display_fn.get_nth_param(1).unwrap().into_pointer_value();

        // `fn to_string(ref self)` takes the receiver by pointer, which is what
        // this signature already hands us. A receiver lowered BY VALUE (a
        // scalar-shaped type) needs the load instead, so branch on the callee's
        // own parameter type rather than assuming either shape.
        let recv: BasicMetadataValueEnum<'ctx> = match user_fn.get_type().get_param_types().first()
        {
            Some(t) if !t.is_pointer_type() => {
                let bt = BasicTypeEnum::try_from(*t)
                    .expect("emit_user_display_impl_fn: receiver is a basic type");
                self.builder
                    .build_load(bt, val_ptr, "ud.recv")
                    .unwrap()
                    .into()
            }
            _ => val_ptr.into(),
        };
        let sval = self
            .builder
            .build_call(user_fn, &[recv], "ud.s")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_struct_value();
        let data = self
            .builder
            .build_extract_value(sval, 0, "ud.data")
            .unwrap()
            .into_pointer_value();
        let len = self
            .builder
            .build_extract_value(sval, 1, "ud.len")
            .unwrap()
            .into_int_value();
        let cap = self
            .builder
            .build_extract_value(sval, 2, "ud.cap")
            .unwrap()
            .into_int_value();
        self.emit_string_append_raw(acc, data, len);

        // Free only an OWNING String — `cap == 0 ⇔ non-owning`. A user
        // `to_string` that returns a string LITERAL (`match self { B => "bee" }`)
        // points `data` at a read-only global and carries `cap == 0`; freeing
        // that aborts. Same invariant and same guard as
        // `emit_write_and_free_string`, which the `println` path uses.
        let fn_val = self.current_fn.unwrap();
        let do_free = self.context.append_basic_block(fn_val, "ud.free");
        let after = self.context.append_basic_block(fn_val, "ud.after");
        let owns = self
            .builder
            .build_int_compare(
                IntPredicate::UGT,
                cap,
                self.context.i64_type().const_zero(),
                "ud.owns",
            )
            .unwrap();
        self.builder
            .build_conditional_branch(owns, do_free, after)
            .unwrap();
        self.builder.position_at_end(do_free);
        self.builder
            .build_call(self.runtime_fns.free_fn, &[data.into()], "")
            .unwrap();
        self.builder.build_unconditional_branch(after).unwrap();

        self.builder.position_at_end(after);
        self.builder.build_return(None).unwrap();

        self.current_fn = saved_fn;
        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        display_fn
    }

    pub(super) fn expr_user_struct_name(&self, expr: &Expr) -> Option<String> {
        match &expr.kind {
            ExprKind::Identifier(n) => self
                .var_types
                .var_type_names
                .get(n.as_str())
                .filter(|tn| self.type_decls.struct_field_names.contains_key(tn.as_str()))
                .cloned(),
            // `self` receiver — registered under the name "self" in
            // `var_type_names`. Recognising it here lets a struct
            // `self.to_string()` / `f"{self}"` render under codegen, ref-aware
            // (the render helper builds `self.field` accesses). This was gated
            // out while the struct-`.to_string()` return-position double-free was
            // live (B-2026-07-12-17); now that its fstr-acc ownership transfer is
            // fixed at return positions, the struct self receiver is safe too
            // (mirrors the enum helpers, B-2026-07-12-15).
            ExprKind::SelfValue => self
                .var_types
                .var_type_names
                .get("self")
                .filter(|tn| self.type_decls.struct_field_names.contains_key(tn.as_str()))
                .cloned(),
            ExprKind::FieldAccess { object, field } => {
                let outer = self.expr_user_struct_name(object)?;
                let tes = self.type_decls.struct_field_type_exprs.get(&outer)?;
                let names = self.type_decls.struct_field_names.get(&outer)?;
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
                .var_types
                .var_type_names
                .get(n.as_str())
                .filter(|tn| self.type_decls.enum_unit_variants.contains_key(tn.as_str()))
                .cloned(),
            // `self` receiver — see the note in `expr_user_struct_name`.
            ExprKind::SelfValue => self
                .var_types
                .var_type_names
                .get("self")
                .filter(|tn| self.type_decls.enum_unit_variants.contains_key(tn.as_str()))
                .cloned(),
            ExprKind::FieldAccess { object, field } => {
                let outer = self.expr_user_struct_name(object)?;
                let tes = self.type_decls.struct_field_type_exprs.get(&outer)?;
                let names = self.type_decls.struct_field_names.get(&outer)?;
                let idx = names.iter().position(|f| f == field)?;
                if let crate::ast::TypeKind::Path(p) = &tes.get(idx)?.kind {
                    if let Some(seg) = p.segments.last() {
                        if self.type_decls.enum_unit_variants.contains_key(seg) {
                            return Some(seg.clone());
                        }
                    }
                }
                None
            }
            // A unit-variant PATH used directly as the operand —
            // `f"{Direction.Up}"` / `println(Status.Closed)`, which is the
            // form design.md § derive(Display) on enums teaches
            // (B-2026-08-17-34). The parser emits it as a 2-segment `Path`,
            // not an `Identifier`, so the place-expression arms above never
            // saw it and the operand fell through to the struct-shaped
            // refusal further down — a diagnostic that then misnamed the
            // program ("bind a struct literal or call result to a `let`"
            // describes neither). `compile_unit_enum_display` compiles the
            // operand through the ordinary path lowering, so recognizing the
            // shape here is the whole fix.
            //
            // Guarded exactly as `compile_path_expr` guards its own
            // enum-variant arm: a leading segment that names a local variable
            // or module binding is a value-rooted field path (`CFG.max`), not
            // a variant, and must keep falling through.
            ExprKind::Path { segments, .. } if segments.len() == 2 => {
                let (head, variant) = (&segments[0], &segments[1]);
                if self.variables.contains_key(head)
                    || self.mod_bindings.module_bindings.contains_key(head)
                {
                    return None;
                }
                let variants = self.type_decls.enum_unit_variants.get(head.as_str())?;
                variants.contains(variant).then(|| head.clone())
            }
            _ => None,
        }
    }

    /// The DISPLAY spelling of `variant` for `enum_name` — the variant name as
    /// written, or its `snake_case` form when the enum carries
    /// `#[derive(Display(snake_case))]`.
    ///
    /// B-2026-08-17-34: codegen rendered the raw variant name unconditionally
    /// while the interpreter applied the flag, so the same program printed
    /// `fast_path` under `--interp` and `FastPath` under both compiled
    /// backends — a silent run-vs-build divergence on a documented derive
    /// option. Both the attribute predicate (`has_display_snake_case`) and the
    /// case transform (`pascal_to_snake`) are the interpreter's own, reused
    /// here rather than re-implemented, so the two backends cannot drift.
    pub(super) fn enum_display_variant_name(&self, enum_name: &str, variant: &str) -> String {
        let snake = |items: &[Item]| -> Option<bool> {
            items.iter().find_map(|it| match it {
                Item::EnumDef(e) if e.name == enum_name => {
                    Some(crate::typechecker::has_display_snake_case(&e.attributes))
                }
                _ => None,
            })
        };
        let is_snake = self
            .program_snapshot
            .as_ref()
            .and_then(|p| snake(&p.items))
            .or_else(|| {
                crate::prelude::STDLIB_PROGRAMS
                    .iter()
                    .find_map(|(_, p)| snake(&p.items))
            })
            .unwrap_or(false);
        if is_snake {
            crate::interpreter::pascal_to_snake(variant)
        } else {
            variant.to_string()
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
            .type_decls
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
                .type_decls
                .enum_layouts
                .get(enum_name)
                .and_then(|l| l.tags.get(vname))
                .ok_or_else(|| format!("Display: missing tag for {enum_name}.{vname}"))?;
            // Display spelling, not the declared spelling — honours
            // `#[derive(Display(snake_case))]` (B-2026-08-17-34).
            let disp = self.enum_display_variant_name(enum_name, vname);
            let g = self
                .builder
                .build_global_string_ptr(&disp, "enumv")
                .unwrap()
                .as_pointer_value();
            let l = i64_t.const_int(disp.len() as u64, false);
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
    /// `args` are the enum's CONCRETE generic arguments at this use site, empty
    /// for a non-generic enum.
    ///
    /// B-2026-08-19-28. Without them this rendered from the DECLARATION, so a
    /// generic enum's payload type was the bare parameter (`T`) — which has no
    /// layout and no renderer, and recursing on it panicked the compiler with
    /// `type_name 'T' not yet supported`. That hit every generic enum reached
    /// through a container or field: `println(v)` on a `Vec[Option[i64]]`, a
    /// `Map` value, a struct field, and the user-defined generics too. The
    /// seeded `Option`/`Result` appeared to work only because a bespoke
    /// instantiation-aware path (`emit_option_display_te`) intercepts the
    /// DIRECT `println(o)` spelling ahead of this function and happens to leave
    /// a concrete fn in the cache that a later nested use then finds — which is
    /// why the panic was order-dependent, and why deleting an unrelated
    /// `println` could break a build.
    ///
    /// Substituting here fixes all of those in one place, rather than adding a
    /// second special case for `Option`/`Result` and leaving user generics
    /// broken. The substitution helper is the one the drop synthesizer already
    /// uses for generic struct fields.
    /// Replace a declaration's generic parameters with the use site's concrete
    /// arguments. A no-op when `subst` is empty (a non-generic enum), so the
    /// non-generic path allocates nothing extra.
    fn display_subst_te(
        te: &TypeExpr,
        subst: &std::collections::HashMap<String, TypeExpr>,
    ) -> TypeExpr {
        if subst.is_empty() {
            return te.clone();
        }
        crate::codegen::helpers::subst_type_params_in_type_expr(te, subst)
    }

    pub(super) fn emit_enum_display_fn(
        &mut self,
        enum_name: &str,
        args: &[GenericArg],
    ) -> FunctionValue<'ctx> {
        self.emit_enum_display_fn_keyed(enum_name, args, "")
    }

    /// `emit_enum_display_fn` with an explicit IDENTITY SUFFIX appended to the
    /// cache key and the symbol name.
    ///
    /// The suffix separates "which enum's variants do I render" (`enum_name`,
    /// which still drives every layout and AST lookup) from "who owns this
    /// cache slot". Every ordinary caller passes `""` and gets exactly the
    /// behaviour it always had.
    ///
    /// It exists for `emit_shared_enum_debug_display_fn` (B-2026-08-24-9). That
    /// wrapper takes a HANDLE and delegates to this function, which takes the
    /// AGGREGATE — two functions rendering one enum, with different first
    /// parameters. Sharing the bare enum name between them made a
    /// self-referential `shared enum` unrenderable either way round: with the
    /// inner registered first, a payload of the enum's own type resolved to the
    /// aggregate-taking inner and was handed a handle (a WRONG VARIANT, not a
    /// crash); with the wrapper registered first, this function's own entry
    /// check found the wrapper and emitted a self-call, hanging the binary.
    /// Giving the inner its own key lets the wrapper own the bare name, which
    /// is what every nested lookup should resolve to, since a payload of that
    /// type is a handle.
    pub(super) fn emit_enum_display_fn_keyed(
        &mut self,
        enum_name: &str,
        args: &[GenericArg],
        key_suffix: &str,
    ) -> FunctionValue<'ctx> {
        // Per-INSTANTIATION identity: `MyOpt[i64]` and `MyOpt[String]` render
        // differently, so they cannot share one fn. A non-generic enum keeps
        // the bare name it has always had, so nothing about those changes.
        let params = self.enum_generic_param_names(enum_name);
        let mut subst: std::collections::HashMap<String, TypeExpr> =
            std::collections::HashMap::new();
        for (p, a) in params.iter().zip(args.iter()) {
            if let GenericArg::Type(t) = a {
                subst.insert(p.clone(), t.clone());
            }
        }
        let suffix: String = if subst.is_empty() {
            String::new()
        } else {
            params
                .iter()
                .map(|p| match subst.get(p) {
                    Some(t) => format!("_{}", Self::display_mangle_te(t)),
                    None => format!("_{p}"),
                })
                .collect()
        };
        let cache_key = format!("{enum_name}{suffix}{key_suffix}");
        if let Some(f) = self.disp_cache_get(&cache_key) {
            return f;
        }
        let fn_name = format!("{}{cache_key}", self.disp_sym_prefix());
        if let Some(f) = self.module.get_function(&fn_name) {
            self.disp_cache_put(cache_key, f);
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
            .type_decls
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
        // `disp_cache_put`, not a bare `display_fn_cache.insert`: the lookup at
        // the top of this function is mode-aware, so inserting into the display
        // map unconditionally meant a fn synthesized in DEBUG mode was filed
        // under the DISPLAY map, where a later `println` of the same enum would
        // find it and render Debug's quoted leaves. Harmless while nothing
        // reached both modes for one enum; wrong the moment something did.
        self.disp_cache_put(cache_key.clone(), display_fn);

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
                        let field_te = Self::display_subst_te(field_te, &subst);
                        self.emit_enum_field_display(agg, &offsets, i, &field_te, acc, display_fn);
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
                        let field_te = Self::display_subst_te(&sf.ty, &subst);
                        self.emit_enum_field_display(agg, &offsets, i, &field_te, acc, display_fn);
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
        // ALL of the field's words, not the first three. The three-word form
        // was the only entry point here, so a payload wider than three words
        // reconstructed its first three components and left the rest undef —
        // a `Vector[i64, 4]` rendered `Vector(1, 2, 3, 51)`, three correct
        // lanes and one garbage (B-2026-08-31-18). `num` is the variant's
        // reserved span for this field, which the layout pass and the pack
        // side both compute from the same word count.
        let field_words: Vec<inkwell::values::IntValue<'ctx>> =
            (0..num.max(1)).map(|j| word(self, j)).collect();
        let w0 = word(self, 0);
        let field_ty = self.llvm_type_for_type_expr(field_te);
        // OVERSIZED payload: the pack side heap-boxed it and stored the box
        // pointer in word 0, because the variant's inline area is narrower than
        // the value. That is the normal state for a GENERIC enum, whose layout
        // is the erased base — `MyOpt[String]`'s `Has` has a one-word slot while
        // a String is three. `word()` above zero-fills past `num`, so reading it
        // inline rebuilt `{ptr, 0, 0}` and rendered an EMPTY string: `Has()`
        // where the interpreter said `Has(x)`.
        //
        // The predicate and the recovery are the same ones the match-arm unpack
        // uses (`reconstruct_payload_value`), so both sites agree about which
        // payloads are boxed — it is a pure function of the static type
        // (B-2026-08-19-28). Before this function learned to substitute generic
        // parameters at all, this shape could not be reached: it panicked on the
        // bare `T` instead.
        let want = Self::llvm_type_word_count(field_ty);
        let field_val = if want > num {
            let ptr_ty = self.context.ptr_type(AddressSpace::default());
            let box_ptr = self
                .builder
                .build_int_to_ptr(w0, ptr_ty, "enum.fbox.p")
                .unwrap();
            let ld = self
                .builder
                .build_load(field_ty, box_ptr, "enum.fbox.ld")
                .unwrap();
            // Relaxed to malloc's guarantee, like every other box consumer
            // (B-2026-08-31-18).
            if let Some(inst) = ld.as_instruction_value() {
                let _ = inst.set_alignment(8);
            }
            ld
        } else {
            self.rebuild_value_from_payload_word_slice(field_ty, &field_words)
                .unwrap_or_else(|_| w0.into())
        };
        let slot = self.create_entry_alloca(display_fn, "enum.field", field_val.get_type());
        self.builder.build_store(slot, field_val).unwrap();
        let field_disp = self.emit_display_fn_for_type_expr(field_te);
        self.builder
            .build_call(field_disp, &[slot.into(), acc.into()], "enum.fd")
            .unwrap();
    }

    /// B-2026-07-08-18: append-form DEBUG renderer for a user struct that
    /// appears NESTED inside another type's Display — an enum payload
    /// (Option/Result/user-enum) or a Vec/Map/Set element. Emits `TypeName {
    /// field: <val>, … }`, recursing per field via
    /// `emit_display_fn_for_type_expr` (so primitive / String / nested-struct
    /// fields all render; String fields print UNQUOTED, matching the
    /// interpreter and the `Vec[String]` path). This matches the INTERPRETER's
    /// nested-struct rendering, which uses this debug/field format — NOT the
    /// struct's own `Display` impl (that impl is only the top-level `println(p)`
    /// spelling). Keeping codegen aligned with the interpreter here avoids
    /// introducing a new run-vs-build divergence now that `karac run` is
    /// JIT-default (Slice 6c). The struct value arrives by pointer; fields are
    /// GEP'd in place. Cached under the struct's own display name — a bare
    /// struct's Display goes through `compile_struct_display_string` (a distinct
    /// inline-f-string path), so there is no collision.
    pub(super) fn emit_struct_debug_display_fn(
        &mut self,
        struct_name: &str,
    ) -> FunctionValue<'ctx> {
        let type_name = struct_name.to_string();
        if let Some(f) = self.disp_cache_get(&type_name) {
            return f;
        }
        let fn_name = format!("{}{type_name}", self.disp_sym_prefix());
        if let Some(f) = self.module.get_function(&fn_name) {
            self.disp_cache_put(type_name, f);
            return f;
        }
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let struct_ty = *self
            .type_decls
            .struct_types
            .get(struct_name)
            .expect("emit_struct_debug_display_fn: struct type registered");
        let field_names = self
            .type_decls
            .struct_field_names
            .get(struct_name)
            .cloned()
            .unwrap_or_default();
        let field_tes = self
            .type_decls
            .struct_field_type_exprs
            .get(struct_name)
            .cloned()
            .unwrap_or_default();

        let saved_bb = self.builder.get_insert_block();
        let saved_fn = self.current_fn;
        let display_fn_ty = self
            .context
            .void_type()
            .fn_type(&[ptr_ty.into(), ptr_ty.into()], false);
        let display_fn = self
            .module
            .add_function(&fn_name, display_fn_ty, Some(Linkage::Internal));
        self.disp_cache_put(type_name, display_fn);

        let entry_bb = self.context.append_basic_block(display_fn, "entry");
        self.builder.position_at_end(entry_bb);
        self.current_fn = Some(display_fn);
        let val_ptr = display_fn.get_nth_param(0).unwrap().into_pointer_value();
        let acc = display_fn.get_nth_param(1).unwrap().into_pointer_value();

        self.disp_append_lit(acc, &format!("{struct_name} {{ "));
        for (i, (fname, fte)) in field_names.iter().zip(field_tes.iter()).enumerate() {
            if i > 0 {
                self.disp_append_lit(acc, ", ");
            }
            self.disp_append_lit(acc, &format!("{fname}: "));
            let field_ptr = self
                .builder
                .build_struct_gep(struct_ty, val_ptr, i as u32, "dbg.f")
                .unwrap();
            let field_disp = self.emit_display_fn_for_type_expr(fte);
            self.builder
                .build_call(field_disp, &[field_ptr.into(), acc.into()], "dbg.fd")
                .unwrap();
        }
        self.disp_append_lit(acc, " }");
        // Appends may split the current block (buffer grow) — return from
        // wherever we end up (mirrors the enum/option renderers).
        self.builder.build_return(None).unwrap();

        self.current_fn = saved_fn;
        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        display_fn
    }

    /// B-2026-08-24-6: synthesize a `Debug` renderer for a `shared enum` — the
    /// RC-backed sibling of `emit_enum_display_fn`, and a thin wrapper over it.
    ///
    /// A shared enum's box is `{ i64 strong, <enum words…> }`, so the enum
    /// aggregate the ordinary renderer already knows how to switch on is simply
    /// the box's TAIL. This loads the handle, GEPs past the control header, and
    /// hands the resulting pointer to `emit_enum_display_fn` — no second copy of
    /// the tag dispatch, the variant blocks, or the payload-word extraction.
    ///
    /// REUSING IT THIS WAY IS ONLY SOUND BECAUSE THE TWO LAYOUTS COINCIDE, and
    /// that was measured rather than assumed (the row that filed this flagged it
    /// as the one thing to check): for a four-variant enum with a `String`
    /// payload, `heap_type` is 6 × i64 and the enum's own `llvm_type` is
    /// 5 × i64. Every word is an i64 on both sides, so no padding can differ
    /// and the tail is bit-identical to the aggregate. If a future layout gives
    /// either side a non-i64 field, this GEP-and-load stops being exact and the
    /// symptom would be a WRONG VARIANT NAME rather than a crash — so the
    /// assertion below fails the build instead of rendering a lie.
    ///
    /// THE TWO RENDERERS HAVE SEPARATE CACHE KEYS, and that is what makes a
    /// SELF-REFERENTIAL shared enum work (B-2026-08-24-9). The wrapper owns the
    /// bare enum name — the key `emit_display_fn_for_type_expr` looks up — and
    /// the inner is emitted under `<Enum>__agg`. So a payload of this enum's
    /// own type resolves to the wrapper, which is right: that payload holds a
    /// handle. While the two shared one key, neither order worked — inner-first
    /// rendered a wrong variant (the handle word read as an aggregate) and
    /// wrapper-first emitted a self-call that hung the binary — and cyclic
    /// enums had to be refused outright.
    pub(super) fn emit_shared_enum_debug_display_fn(
        &mut self,
        enum_name: &str,
    ) -> FunctionValue<'ctx> {
        let type_name = enum_name.to_string();
        if let Some(f) = self.disp_cache_get(&type_name) {
            return f;
        }
        let fn_name = format!("{}shared_{type_name}", self.disp_sym_prefix());
        if let Some(f) = self.module.get_function(&fn_name) {
            self.disp_cache_put(type_name, f);
            return f;
        }
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let info = self
            .type_decls
            .shared_types
            .get(enum_name)
            .expect("emit_shared_enum_debug_display_fn: shared type registered");
        let heap_type = info.heap_type;
        let field_base: u32 = if info.has_weak_header { 2 } else { 1 };

        // The layout coincidence this renderer rests on. Checked here rather
        // than trusted: the box must be exactly the control header plus the
        // enum's own words, in order.
        let enum_llvm = self
            .type_decls
            .enum_layouts
            .get(enum_name)
            .expect("emit_shared_enum_debug_display_fn: enum layout registered")
            .llvm_type;
        let box_fields = heap_type.get_field_types();
        let enum_fields = enum_llvm.get_field_types();
        assert_eq!(
            box_fields.len(),
            enum_fields.len() + field_base as usize,
            "shared enum `{enum_name}`: box is not the control header plus the enum's words"
        );
        assert!(
            box_fields
                .iter()
                .skip(field_base as usize)
                .zip(enum_fields.iter())
                .all(|(a, b)| a == b),
            "shared enum `{enum_name}`: box tail does not match the enum aggregate word for word"
        );

        let saved_bb = self.builder.get_insert_block();
        let saved_fn = self.current_fn;
        let display_fn_ty = self
            .context
            .void_type()
            .fn_type(&[ptr_ty.into(), ptr_ty.into()], false);
        let display_fn = self
            .module
            .add_function(&fn_name, display_fn_ty, Some(Linkage::Internal));
        // Register the wrapper under the BARE enum name BEFORE synthesizing the
        // inner renderer, and give the inner its own key (B-2026-08-24-9). Both
        // halves matter:
        //   - wrapper first, so that a payload of this enum's OWN type, looked
        //     up while the inner is being emitted, resolves to the handle-taking
        //     wrapper. That payload IS a handle, so the wrapper is the correct
        //     answer; resolving to the inner instead read the handle word as an
        //     enum aggregate and printed a wrong variant.
        //   - a distinct inner key, so that registering the wrapper first does
        //     not make `emit_enum_display_fn`'s own entry check find the wrapper
        //     and emit a self-call — which compiled cleanly and hung the binary.
        // Sharing one key made those two requirements contradictory, which is
        // why self-referential shared enums were refused until now.
        self.disp_cache_put(type_name, display_fn);
        let inner = self.emit_enum_display_fn_keyed(enum_name, &[], "__agg");

        let entry_bb = self.context.append_basic_block(display_fn, "entry");
        self.builder.position_at_end(entry_bb);
        self.current_fn = Some(display_fn);
        let slot_ptr = display_fn.get_nth_param(0).unwrap().into_pointer_value();
        let acc = display_fn.get_nth_param(1).unwrap().into_pointer_value();
        let handle = self
            .builder
            .build_load(ptr_ty, slot_ptr, "shenum.handle")
            .unwrap()
            .into_pointer_value();
        let agg_ptr = self
            .builder
            .build_struct_gep(heap_type, handle, field_base, "shenum.agg")
            .unwrap();
        self.builder
            .build_call(inner, &[agg_ptr.into(), acc.into()], "shenum.d")
            .unwrap();
        self.builder.build_return(None).unwrap();

        self.current_fn = saved_fn;
        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        display_fn
    }

    /// B-2026-08-24-2: synthesize `void karac_display_<Sh>(ptr val, ptr acc)` for
    /// a `shared struct`, the RC-backed sibling of `emit_struct_debug_display_fn`.
    ///
    /// Two things differ from the plain-struct renderer. First, `val` points at
    /// a slot holding the HANDLE, not at the fields, so the body loads the
    /// pointer and GEPs into the heap box. Second, user field 0 does not sit at
    /// heap index 0: the box is `{ i64 strong, fields… }`, or
    /// `{ i64 strong, i64 weak, fields… }` when the type is the target of a
    /// `weak` field anywhere in the program. `field_base` is computed from
    /// `has_weak_header` directly rather than through `shared_gep_layout` —
    /// that funnel also answers for the HEADERLESS niche (base 0), and one of
    /// the two sets behind it, `headerless_fns`, is keyed per `(fn, type)`
    /// (`headerless_here` reads `fn_ctx.current_fn_name`) while this renderer
    /// is emitted once and cached program-wide. Baking a per-function layout
    /// into a shared function is how a field read lands at the wrong offset,
    /// so the caller refuses a headerless type instead. Same reasoning
    /// `synth_drop` records for its own `heap_type`-based walk.
    ///
    /// The other set, `headerless_types`, IS program-wide and uniform, so a
    /// program-wide renderer could serve it by taking its base from the funnel
    /// — that half needs no cache-key work. Neither is built, because neither
    /// is reachable: `dbg` of a shared value demotes the type out of both sets,
    /// measured in B-2026-08-24-18 and pinned by `tests/elision.rs`. Building
    /// an untestable GEP path is the hazard, not the fix.
    ///
    /// Field ORDER is `struct_field_names`, i.e. declaration order — the same
    /// order the interpreter's `render_typed_mode` now walks, which is what
    /// makes the two backends agree by construction rather than by convention.
    pub(super) fn emit_shared_struct_debug_display_fn(
        &mut self,
        struct_name: &str,
    ) -> FunctionValue<'ctx> {
        let type_name = struct_name.to_string();
        if let Some(f) = self.disp_cache_get(&type_name) {
            return f;
        }
        let fn_name = format!("{}{type_name}", self.disp_sym_prefix());
        if let Some(f) = self.module.get_function(&fn_name) {
            self.disp_cache_put(type_name, f);
            return f;
        }
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let info = self
            .type_decls
            .shared_types
            .get(struct_name)
            .expect("emit_shared_struct_debug_display_fn: shared type registered");
        let heap_type = info.heap_type;
        let field_base: u32 = if info.has_weak_header { 2 } else { 1 };
        let niche_fields = info.niche_option_fields.clone();
        let field_names = self
            .type_decls
            .struct_field_names
            .get(struct_name)
            .cloned()
            .unwrap_or_default();
        let field_tes = self
            .type_decls
            .struct_field_type_exprs
            .get(struct_name)
            .cloned()
            .unwrap_or_default();

        let saved_bb = self.builder.get_insert_block();
        let saved_fn = self.current_fn;
        let display_fn_ty = self
            .context
            .void_type()
            .fn_type(&[ptr_ty.into(), ptr_ty.into()], false);
        let display_fn = self
            .module
            .add_function(&fn_name, display_fn_ty, Some(Linkage::Internal));
        // Cached BEFORE the body is emitted, so a self-referential shape
        // (`Link { next: Option[Link] }`) recurses into this same function
        // instead of synthesizing forever.
        self.disp_cache_put(type_name, display_fn);

        let entry_bb = self.context.append_basic_block(display_fn, "entry");
        self.builder.position_at_end(entry_bb);
        self.current_fn = Some(display_fn);
        let slot_ptr = display_fn.get_nth_param(0).unwrap().into_pointer_value();
        let acc = display_fn.get_nth_param(1).unwrap().into_pointer_value();
        let handle = self
            .builder
            .build_load(ptr_ty, slot_ptr, "sh.handle")
            .unwrap()
            .into_pointer_value();

        self.disp_append_lit(acc, &format!("{struct_name} {{ "));
        for (i, fname) in field_names.iter().enumerate() {
            if i > 0 {
                self.disp_append_lit(acc, ", ");
            }
            self.disp_append_lit(acc, &format!("{fname}: "));
            let field_ptr = self
                .builder
                .build_struct_gep(heap_type, handle, field_base + i as u32, "sh.f")
                .unwrap();
            match niche_fields.get(i).and_then(|n| n.clone()) {
                // Niche `Option[shared Inner]`: the slot holds a bare pointer,
                // null for `None`, rather than the conventional 4-word Option
                // aggregate — so the ordinary Option renderer would read three
                // words past the end of a one-word field. Rendered here to match
                // the interpreter's `Some(<inner>)` / `None` spelling.
                Some(inner) => {
                    let inner_fn = self.emit_shared_struct_debug_display_fn(&inner);
                    let loaded = self
                        .builder
                        .build_load(ptr_ty, field_ptr, "sh.niche")
                        .unwrap()
                        .into_pointer_value();
                    let is_null = self.builder.build_is_null(loaded, "sh.niche.null").unwrap();
                    let none_bb = self.context.append_basic_block(display_fn, "sh.none");
                    let some_bb = self.context.append_basic_block(display_fn, "sh.some");
                    let join_bb = self.context.append_basic_block(display_fn, "sh.join");
                    self.builder
                        .build_conditional_branch(is_null, none_bb, some_bb)
                        .unwrap();

                    self.builder.position_at_end(none_bb);
                    self.disp_append_lit(acc, "None");
                    self.builder.build_unconditional_branch(join_bb).unwrap();

                    // An append may split the current block (buffer grow), so
                    // branch to the join from wherever we end up — the same
                    // rule the enum / Option renderers follow.
                    self.builder.position_at_end(some_bb);
                    self.disp_append_lit(acc, "Some(");
                    self.builder
                        .build_call(inner_fn, &[field_ptr.into(), acc.into()], "sh.fd")
                        .unwrap();
                    self.disp_append_lit(acc, ")");
                    self.builder.build_unconditional_branch(join_bb).unwrap();

                    self.builder.position_at_end(join_bb);
                }
                None => {
                    let fte = field_tes.get(i).cloned().unwrap_or_else(|| TypeExpr {
                        kind: TypeKind::Path(PathExpr {
                            segments: vec!["i64".to_string()],
                            generic_args: None,
                            span: crate::token::Span::default(),
                        }),
                        span: crate::token::Span::default(),
                    });
                    let field_disp = self.emit_display_fn_for_type_expr(&fte);
                    self.builder
                        .build_call(field_disp, &[field_ptr.into(), acc.into()], "sh.fd")
                        .unwrap();
                }
            }
        }
        self.disp_append_lit(acc, " }");
        self.builder.build_return(None).unwrap();

        self.current_fn = saved_fn;
        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        display_fn
    }

    /// B-2026-07-08-9: synthesize `void karac_display_Option_<T>(ptr val, ptr acc)`
    /// for a CONCRETE payload type `payload_te`, appending `Some(<T display>)` /
    /// `None` to the String accumulator `acc`. `Option` is a generic built-in,
    /// so `emit_enum_display_fn` (which reads the generic `Some(T)` variant def)
    /// can't render it — this bakes the concrete payload type in, recovering the
    /// missing plumbing that left Option Display unsupported in codegen while the
    /// interpreter rendered `Some(x)` / `None`. Cached per mangled payload type;
    /// reuses `emit_enum_field_display` for the payload word extraction +
    /// recursion. Matches the interpreter's `Some(x)` / `None` spelling.
    pub(super) fn emit_option_display_te(&mut self, payload_te: &TypeExpr) -> FunctionValue<'ctx> {
        let mangled = Self::display_mangle_te(payload_te);
        let type_name = format!("Option_{mangled}");
        if let Some(f) = self.disp_cache_get(&type_name) {
            return f;
        }
        let fn_name = format!("{}{type_name}", self.disp_sym_prefix());
        if let Some(f) = self.module.get_function(&fn_name) {
            self.disp_cache_put(type_name, f);
            return f;
        }
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();
        let layout = self
            .type_decls
            .enum_layouts
            .get("Option")
            .expect("emit_option_display_te: Option layout seeded");
        let llvm_ty = layout.llvm_type;
        let some_tag = layout.tags.get("Some").copied().unwrap_or(1);
        let some_offsets = layout
            .field_word_offsets
            .get("Some")
            .cloned()
            .unwrap_or_else(|| vec![(0, 3)]);

        let saved_bb = self.builder.get_insert_block();
        let saved_fn = self.current_fn;
        let display_fn_ty = self
            .context
            .void_type()
            .fn_type(&[ptr_ty.into(), ptr_ty.into()], false);
        let display_fn = self
            .module
            .add_function(&fn_name, display_fn_ty, Some(Linkage::Internal));
        self.disp_cache_put(type_name, display_fn);

        let entry_bb = self.context.append_basic_block(display_fn, "entry");
        self.builder.position_at_end(entry_bb);
        self.current_fn = Some(display_fn);
        let val_ptr = display_fn.get_nth_param(0).unwrap().into_pointer_value();
        let acc = display_fn.get_nth_param(1).unwrap().into_pointer_value();
        let agg = self
            .builder
            .build_load(llvm_ty, val_ptr, "opt.agg")
            .unwrap()
            .into_struct_value();
        let tag = self
            .builder
            .build_extract_value(agg, 0, "opt.tag")
            .unwrap()
            .into_int_value();

        let some_bb = self.context.append_basic_block(display_fn, "opt.some");
        let none_bb = self.context.append_basic_block(display_fn, "opt.none");
        let exit_bb = self.context.append_basic_block(display_fn, "opt.exit");
        let is_some = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                tag,
                i64_t.const_int(some_tag, false),
                "opt.is_some",
            )
            .unwrap();
        self.builder
            .build_conditional_branch(is_some, some_bb, none_bb)
            .unwrap();

        // None
        self.builder.position_at_end(none_bb);
        self.disp_append_lit(acc, "None");
        self.builder.build_unconditional_branch(exit_bb).unwrap();

        // Some(<payload>) — an append may split the current block (buffer grow),
        // so branch to exit from wherever we end up (mirrors emit_enum_display_fn).
        self.builder.position_at_end(some_bb);
        self.disp_append_lit(acc, "Some(");
        self.emit_enum_field_display(agg, &some_offsets, 0, payload_te, acc, display_fn);
        self.disp_append_lit(acc, ")");
        self.builder.build_unconditional_branch(exit_bb).unwrap();

        self.builder.position_at_end(exit_bb);
        self.builder.build_return(None).unwrap();

        self.current_fn = saved_fn;
        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        display_fn
    }

    /// B-2026-07-08-9 sibling: `void karac_display_Result_<T>_<E>(ptr val, ptr acc)`
    /// rendering `Ok(<T display>)` / `Err(<E display>)` for concrete `(ok, err)`
    /// payload types. Same rationale + structure as `emit_option_display_te`.
    /// (Payload extraction reuses `emit_enum_field_display`, which reconstructs
    /// up to a 3-word payload — covers primitives and String; wider struct
    /// payloads are a follow-on.)
    pub(super) fn emit_result_display_te(
        &mut self,
        ok_te: &TypeExpr,
        err_te: &TypeExpr,
    ) -> FunctionValue<'ctx> {
        let type_name = format!(
            "Result_{}_{}",
            Self::display_mangle_te(ok_te),
            Self::display_mangle_te(err_te)
        );
        if let Some(f) = self.disp_cache_get(&type_name) {
            return f;
        }
        let fn_name = format!("{}{type_name}", self.disp_sym_prefix());
        if let Some(f) = self.module.get_function(&fn_name) {
            self.disp_cache_put(type_name, f);
            return f;
        }
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();
        let layout = self
            .type_decls
            .enum_layouts
            .get("Result")
            .expect("emit_result_display_te: Result layout seeded");
        let llvm_ty = layout.llvm_type;
        let ok_tag = layout.tags.get("Ok").copied().unwrap_or(1);
        let ok_offsets = layout
            .field_word_offsets
            .get("Ok")
            .cloned()
            .unwrap_or_else(|| vec![(0, 5)]);
        let err_offsets = layout
            .field_word_offsets
            .get("Err")
            .cloned()
            .unwrap_or_else(|| vec![(0, 5)]);

        let saved_bb = self.builder.get_insert_block();
        let saved_fn = self.current_fn;
        let display_fn_ty = self
            .context
            .void_type()
            .fn_type(&[ptr_ty.into(), ptr_ty.into()], false);
        let display_fn = self
            .module
            .add_function(&fn_name, display_fn_ty, Some(Linkage::Internal));
        self.disp_cache_put(type_name, display_fn);

        let entry_bb = self.context.append_basic_block(display_fn, "entry");
        self.builder.position_at_end(entry_bb);
        self.current_fn = Some(display_fn);
        let val_ptr = display_fn.get_nth_param(0).unwrap().into_pointer_value();
        let acc = display_fn.get_nth_param(1).unwrap().into_pointer_value();
        let agg = self
            .builder
            .build_load(llvm_ty, val_ptr, "res.agg")
            .unwrap()
            .into_struct_value();
        let tag = self
            .builder
            .build_extract_value(agg, 0, "res.tag")
            .unwrap()
            .into_int_value();

        let ok_bb = self.context.append_basic_block(display_fn, "res.ok");
        let err_bb = self.context.append_basic_block(display_fn, "res.err");
        let exit_bb = self.context.append_basic_block(display_fn, "res.exit");
        let is_ok = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                tag,
                i64_t.const_int(ok_tag, false),
                "res.is_ok",
            )
            .unwrap();
        self.builder
            .build_conditional_branch(is_ok, ok_bb, err_bb)
            .unwrap();

        self.builder.position_at_end(ok_bb);
        self.disp_append_lit(acc, "Ok(");
        self.emit_enum_field_display(agg, &ok_offsets, 0, ok_te, acc, display_fn);
        self.disp_append_lit(acc, ")");
        self.builder.build_unconditional_branch(exit_bb).unwrap();

        self.builder.position_at_end(err_bb);
        self.disp_append_lit(acc, "Err(");
        self.emit_enum_field_display(agg, &err_offsets, 0, err_te, acc, display_fn);
        self.disp_append_lit(acc, ")");
        self.builder.build_unconditional_branch(exit_bb).unwrap();

        self.builder.position_at_end(exit_bb);
        self.builder.build_return(None).unwrap();

        self.current_fn = saved_fn;
        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        display_fn
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
            ExprKind::Identifier(n) => self.var_types.var_type_names.get(n.as_str()).cloned(),
            // `self` receiver — see the note in `expr_user_struct_name`.
            ExprKind::SelfValue => self.var_types.var_type_names.get("self").cloned(),
            ExprKind::FieldAccess { object, field } => {
                let outer = self.expr_user_struct_name(object)?;
                let tes = self.type_decls.struct_field_type_exprs.get(&outer)?;
                let names = self.type_decls.struct_field_names.get(&outer)?;
                let idx = names.iter().position(|f| f == field)?;
                if let crate::ast::TypeKind::Path(p) = &tes.get(idx)?.kind {
                    p.segments.last().cloned()
                } else {
                    None
                }
            }
            // A variant PATH operand on a PAYLOAD-BEARING enum —
            // `f"{Evt.MouseUp}"` where `Evt` also has tuple/struct variants.
            // The all-unit twin of this arm lives in `expr_user_enum_name`;
            // both were missing, so every variant-path operand fell through
            // to the struct-shaped refusal (B-2026-08-17-34). Membership is
            // checked against the layout's tag map so a non-variant path on
            // an enum-named head keeps falling through, and a variable- or
            // module-binding-rooted path stays a field read, exactly as
            // `compile_path_expr` treats it.
            ExprKind::Path { segments, .. } if segments.len() == 2 => {
                let (head, variant) = (&segments[0], &segments[1]);
                if self.variables.contains_key(head)
                    || self.mod_bindings.module_bindings.contains_key(head)
                {
                    return None;
                }
                self.type_decls
                    .enum_layouts
                    .get(head.as_str())
                    .filter(|l| l.tags.contains_key(variant))
                    .map(|_| head.clone())
            }
            _ => None,
        }?;
        if self.type_decls.enum_layouts.contains_key(&tn)
            && (!self.type_decls.seeded_enum_names.contains(&tn)
                || self.display.baked_display_enum_names.contains(&tn))
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
        // A GENERIC enum needs its concrete arguments here: the Display fn is
        // synthesized from the declaration, whose payload types are bare
        // parameters, and rendering one panics ("type_name 'T' not yet
        // supported"). The nested spellings get them from the element/field
        // `TypeExpr` they already hold (B-2026-08-19-28); this DIRECT path has
        // only the enum's base name, so the instantiation comes from the
        // span-keyed table the lowering pass forwards — which covers a binding
        // and a call result alike (B-2026-08-19-30). Empty for a non-generic
        // enum and for any span the table does not carry, which is exactly the
        // old behaviour.
        let args: Vec<GenericArg> = self
            .display
            .display_generic_enum_types
            .get(&(expr.span.offset, expr.span.length))
            .and_then(|te| match &te.kind {
                TypeKind::Path(p) => p.generic_args.clone(),
                _ => None,
            })
            .unwrap_or_default();
        let disp = self.emit_enum_display_fn(enum_name, &args);
        // Resolve the enum value's DATA pointer via `get_data_ptr`, not the raw
        // `variables[n].ptr` slot: for a `ref E` param (common when a generic
        // `fn f[E: Display](e: ref E)` monomorphizes to a payload enum) the slot
        // holds a pointer TO the value, so reading the enum from the slot address
        // rendered garbage (`Other()` for every value) under codegen while the
        // interpreter was correct — a build!=run miscompile (B-2026-07-12-18).
        // `get_data_ptr` loads through the ref (and unwraps an RC-promoted
        // binding), and returns the alloca unchanged for an owned binding, so the
        // common `println(local_enum)` case is unaffected.
        let val_ptr = if let ExprKind::Identifier(n) = &expr.kind {
            self.get_data_ptr(n)
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
        // Zero the slot AT ENTRY as well as at the use site (B-2026-08-25-33).
        // The use-site init below is what makes a loop correct, but it only
        // runs on the paths that reach it — and a caller may register this
        // accumulator for FUNCTION-scope cleanup (the `main() -> Result[(), E]`
        // entry-point wrapper does, via `emit_main_result_err_exit`'s f-string
        // bridge). That drain runs on every exit path, including the ones that
        // never rendered anything, so without the entry store the success path
        // reads an uninitialized `cap`, sees garbage > 0, and frees a garbage
        // pointer. `fn main() -> Result[(), AllocError] { let x = ok()?; … }`
        // segfaulted under `karac run` for exactly this reason; AOT survived
        // only because `main`'s frame lands on fresh, zeroed kernel stack
        // pages, while under the JIT it sits on stack the LLVM compilation
        // machinery has already dirtied. Same defect, same fix, as the
        // f-string accumulator this helper was written for.
        self.zero_init_str_acc_at_entry(acc);
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
        if self.var_types.vec_elem_types.contains_key(&name)
            && self.var_types.var_elem_type_exprs.contains_key(&name)
        {
            let elem_te = self.var_types.var_elem_type_exprs[&name].clone();
            let disp = self.emit_vec_display_fn_te(&elem_te);
            return Ok(Some(self.render_via_display_fn(disp, slot.ptr)));
        }
        if self.mapset.map_key_type_exprs.contains_key(&name)
            && self.var_types.var_elem_type_exprs.contains_key(&name)
        {
            let k = self.mapset.map_key_type_exprs[&name].clone();
            let v = self.var_types.var_elem_type_exprs[&name].clone();
            // B-2026-08-14-35 — `SortedMap` shares `Map`'s registries and its
            // storage, so without this test it rendered through the unsorted
            // fn: `Map`'s prefix over `Map`'s bucket order.
            let disp = if self.mapset.sorted_collection_vars.contains(&name) {
                self.emit_sorted_map_display_fn(&k, &v)?
            } else {
                self.emit_map_display_fn(&k, &v)
            };
            return Ok(Some(self.render_via_display_fn(disp, slot.ptr)));
        }
        if self.mapset.set_elem_type_exprs.contains_key(&name) {
            let elem_te = self.mapset.set_elem_type_exprs[&name].clone();
            let disp = if self.mapset.sorted_collection_vars.contains(&name) {
                self.emit_sorted_set_display_fn(&elem_te)?
            } else {
                self.emit_set_display_fn(&elem_te)
            };
            return Ok(Some(self.render_via_display_fn(disp, slot.ptr)));
        }
        // B-2026-07-08-9: Option[T] / Result[T, E] place-expr Display. The
        // payload TypeExpr(s) were captured by `register_var_from_type_expr`;
        // synthesize a concrete `Some(<T>)`/`None` (or `Ok`/`Err`) renderer.
        if let Some(payload_te) = self.var_types.var_option_payload_te.get(&name).cloned() {
            let disp = self.emit_option_display_te(&payload_te);
            return Ok(Some(self.render_via_display_fn(disp, slot.ptr)));
        }
        if let Some((ok_te, err_te)) = self.var_types.var_result_payload_te.get(&name).cloned() {
            let disp = self.emit_result_display_te(&ok_te, &err_te);
            return Ok(Some(self.render_via_display_fn(disp, slot.ptr)));
        }
        Ok(None)
    }

    /// B-2026-07-08-9 (call-result half): render an `Option[T]` / `Result[T, E]`
    /// expression that is NOT a plain variable — a call result (`cache.get(1)`),
    /// a method result, etc. — via its concrete per-payload Display fn. The
    /// variable case (`try_compile_collection_display`) keys off the name-addressed
    /// `var_option_payload_te` table populated at the `let` binding; a bare call
    /// has no name, so we key off `display_option_result_types` (span-addressed,
    /// forwarded from `TypeCheckResult.expr_types`). Non-place values are spilled
    /// to an alloca so the Display fn has a pointer to load the 4-word aggregate
    /// from (mirrors `render_user_enum_display`). Returns `None` when the expr is
    /// not Option/Result-typed OR its payload isn't inline-displayable (compound
    /// payloads remain a follow-on) — the caller then falls through to its
    /// existing error path.
    pub(super) fn try_compile_option_result_display(
        &mut self,
        expr: &Expr,
    ) -> Result<Option<(PointerValue<'ctx>, BasicValueEnum<'ctx>)>, String> {
        let key = (expr.span.offset, expr.span.length);
        let Some(full_te) = self.display.display_option_result_types.get(&key).cloned() else {
            return Ok(None);
        };
        // Resolve the Display fn for the concrete payload, guarding to
        // inline-displayable payloads (primitives + String) exactly as the
        // variable path does — the 4-i64 aggregate reload can't reconstruct a
        // boxed/wide-struct payload.
        // A scalar `ref T` payload (a bare `println(v.first())` on a Vec, typed
        // `Option[ref T]`) is peeled to `T` — the borrow is a same-width copy in
        // the payload word, matching `Option[T]`'s layout (B-2026-07-18-24). The
        // variable path applies the identical peel at `let`-binding registration.
        let disp = if let Some(pte) = Self::option_payload_te(&full_te) {
            let pte = Self::peel_scalar_ref_display_payload(&pte);
            if !self.is_reconstructable_display_payload(&pte) {
                return Ok(None);
            }
            self.emit_option_display_te(&pte)
        } else if let Some((ok_te, err_te)) = Self::result_payload_tes(&full_te) {
            let ok_te = Self::peel_scalar_ref_display_payload(&ok_te);
            let err_te = Self::peel_scalar_ref_display_payload(&err_te);
            if !self.is_reconstructable_display_payload(&ok_te)
                || !self.is_reconstructable_display_payload(&err_te)
            {
                return Ok(None);
            }
            self.emit_result_display_te(&ok_te, &err_te)
        } else {
            return Ok(None);
        };
        // Spill non-place values to an alloca so the append-form Display fn has
        // a pointer to load from (a plain variable would already have a slot,
        // but that case is handled earlier by `try_compile_collection_display`;
        // here the expr is a call/method result).
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
                let slot = self.create_entry_alloca(fn_val, "optres.disp.tmp", val.get_type());
                self.builder.build_store(slot, val).unwrap();
                slot
            }
        };
        Ok(Some(self.render_via_display_fn(disp, val_ptr)))
    }

    /// B-2026-07-18-14: render a WHOLE anonymous-tuple value (`f"{t}"` /
    /// `println(t)` where `t: (i64, i64)` / `(i64, String)`) via its
    /// element-wise Display fn (`emit_tuple_display_fn`), matching the
    /// interpreter's `(a, b)` format. Keys off the span-addressed
    /// `display_tuple_types` table (forwarded from the typechecker's
    /// `expr_types`), so a tuple variable and a tuple call-result are handled
    /// uniformly. Non-place values (a call result) are spilled to an alloca so
    /// the append-form Display fn has a pointer to load the aggregate from; a
    /// variable already has a slot. Returns `None` when `e` is not tuple-typed
    /// (caller falls through). Field-index interpolation (`f"{t.0}"`) never
    /// reaches here — it lowers as an ordinary scalar/field part.
    pub(super) fn try_compile_tuple_display(
        &mut self,
        e: &Expr,
    ) -> Result<Option<(PointerValue<'ctx>, BasicValueEnum<'ctx>)>, String> {
        let key = (e.span.offset, e.span.length);
        let Some(tuple_te) = self.display.display_tuple_types.get(&key).cloned() else {
            return Ok(None);
        };
        let TypeKind::Tuple(elems) = &tuple_te.kind else {
            return Ok(None);
        };
        if elems.is_empty() {
            return Ok(None);
        }
        let disp = self.emit_tuple_display_fn(elems);
        // A variable already has a slot; spill a non-place value (call/method
        // result) so the Display fn can GEP the aggregate through a pointer.
        let val_ptr = if let ExprKind::Identifier(n) = &e.kind {
            self.variables.get(n.as_str()).map(|s| s.ptr)
        } else {
            None
        };
        let val_ptr = match val_ptr {
            Some(p) => p,
            None => {
                let val = self.compile_expr(e)?;
                let fn_val = self.current_fn.unwrap();
                let slot = self.create_entry_alloca(fn_val, "tuple.disp.tmp", val.get_type());
                self.builder.build_store(slot, val).unwrap();
                slot
            }
        };
        Ok(Some(self.render_via_display_fn(disp, val_ptr)))
    }

    /// Render a `Vec[T]` expression that is NOT a plain variable — a fresh
    /// literal (`println(vec![1, 2])`), a free-function or method result
    /// (`println(t.shape())`) — via the same per-element Display fn the
    /// identifier path uses.
    ///
    /// The identifier case keys off the name-addressed `var_elem_type_exprs`
    /// (populated at the `let` binding); an unbound expression has no name, so
    /// this keys off the span-addressed `display_vec_types`, forwarded from
    /// `TypeCheckResult.expr_types`. That table is what makes the case
    /// tractable at all: at the LLVM level a `Vec`'s `{ptr, len, cap}`
    /// aggregate is byte-identical to a `String`'s, so without a source-level
    /// type there is nothing to dispatch on — which is exactly why these
    /// expressions used to fall through to the String arm and print garbage
    /// (B-2026-07-28-12).
    ///
    /// A materialized temporary has no other owner, so its slot is registered
    /// for scope cleanup here (`track_vec_var` with the element type) rather
    /// than freed inline. That routes it through the same `FreeVecBuffer`
    /// drain a bound `Vec` uses, which drops per-element heaps — an inline
    /// free of the outer buffer alone leaks every cell of a `Vec[String]`
    /// (LSan: 16 bytes in 4 allocations for two two-element temporaries).
    /// Deferring to scope exit is also what keeps the buffer alive long enough
    /// for an f-string's memcpy, matching the sibling collection paths.
    /// B-2026-08-31-19 — the DEPTH-0 array renderer, sibling of
    /// [`Self::try_compile_vec_display`] and of `try_compile_vector_display`.
    ///
    /// An array reaching the f-string / `println` paths had no arm at all, and
    /// the outcome depended on its element type rather than failing cleanly:
    /// `f"{a}"` on an `Array[i64, 3]` printed `1` — the value-kind fallback
    /// reading the aggregate as a scalar — `println(a)` printed nothing, and an
    /// `Array[String, 2]` printed the first element's raw data POINTER
    /// (`93921800231728`). The interpreter renders all three `[…]`.
    ///
    /// Keys off the span-addressed `display_array_types`, so a bound array, an
    /// array literal and a call result all take the same route. An array is a
    /// VALUE with no heap of its own — its elements are inline — so unlike the
    /// Vec sibling there is nothing to register for cleanup here; if an element
    /// owns heap (`Array[String, N]`), that heap belongs to whatever owns the
    /// array, and Display only ever appends a copy of the bytes.
    pub(super) fn try_compile_array_display(
        &mut self,
        e: &Expr,
    ) -> Result<Option<(PointerValue<'ctx>, BasicValueEnum<'ctx>)>, String> {
        let key = (e.span.offset, e.span.length);
        let Some(arr_te) = self.display.display_array_types.get(&key).cloned() else {
            return Ok(None);
        };
        let (elem_te, n) = match &arr_te.kind {
            TypeKind::Array { element, size } => match &size.kind {
                ExprKind::Integer(v, _) if *v >= 0 => ((**element).clone(), *v as u64),
                _ => return Ok(None),
            },
            _ => return Ok(None),
        };
        let disp = self.emit_array_display_fn(&elem_te, n, &arr_te);
        // A bound array already has a slot; render through it rather than
        // copying the aggregate (which for an `Array[String, N]` would put a
        // second reference to the same buffers on the stack).
        if let ExprKind::Identifier(name) = &e.kind {
            if let Some(slot) = self.variables.get(name.as_str()).copied() {
                return Ok(Some(self.render_via_display_fn(disp, slot.ptr)));
            }
        }
        let val = self.compile_expr(e)?;
        if !val.is_array_value() {
            return Ok(None);
        }
        let fn_val = self.current_fn.unwrap();
        let slot = self.create_entry_alloca(fn_val, "arr.disp.tmp", val.get_type());
        self.builder.build_store(slot, val).unwrap();
        let rendered = self.render_via_display_fn(disp, slot);
        // A FRESH owned array temporary (`f"{mk(n)}"`, an array literal) has no
        // other owner, and its ELEMENTS can own heap — an `Array[String, 2]`
        // holds two `{ptr, len, cap}` triples inline. Nothing else frees them,
        // so the interpolation leaked both per evaluation (LSan: 40 allocations
        // over a 20-iteration loop). Reuses the array-temp drop the `==`
        // operand path already emits (`emit_drop_fn_for_array`), gated on the
        // same `expr_is_fresh_owned_array_temp` predicate so a BOUND array or a
        // place expression — owned elsewhere — is untouched.
        //
        // Emitted AFTER the render, so every read of the array dominates the
        // drop, and IMMEDIATELY rather than at scope exit: a scope-exit
        // registration hoists one entry alloca and would free only the LAST
        // value when the interpolation sits in a loop, which is exactly the
        // shape that surfaced the leak.
        if self.expr_is_fresh_owned_array_temp(e) {
            if let Some(drop_fn) = self.emit_drop_fn_for_array(&elem_te, n as u32) {
                self.builder
                    .build_call(drop_fn, &[slot.into()], "")
                    .unwrap();
            }
        }
        Ok(Some(rendered))
    }

    pub(super) fn try_compile_vec_display(
        &mut self,
        e: &Expr,
    ) -> Result<Option<(PointerValue<'ctx>, BasicValueEnum<'ctx>)>, String> {
        let key = (e.span.offset, e.span.length);
        let Some(elem_te) = self.display.display_vec_types.get(&key).cloned() else {
            return Ok(None);
        };
        // A bound Vec already has a slot and an owner; only an unbound one is
        // compiled (and owned) here.
        if let ExprKind::Identifier(n) = &e.kind {
            if let Some(slot) = self.variables.get(n.as_str()).copied() {
                let disp = self.emit_vec_display_fn_te(&elem_te);
                return Ok(Some(self.render_via_display_fn(disp, slot.ptr)));
            }
        }
        let val = self.compile_expr(e)?;
        // A bare array-literal binding compiles to an `[N x T]` aggregate
        // rather than the 3-word Vec struct; it has no data pointer to render
        // through, so leave it to the existing paths.
        if !val.is_struct_value() || val.into_struct_value().get_type().count_fields() != 3 {
            return Ok(None);
        }
        let fn_val = self.current_fn.unwrap();
        let slot = self.create_entry_alloca(fn_val, "vec.disp.tmp", val.get_type());
        self.builder.build_store(slot, val).unwrap();
        // B-2026-08-14-30 — register the slot for cleanup only when this
        // expression PRODUCED the Vec. The identifier arm above handles a bound
        // one, and the reasoning here was that everything else must therefore be
        // a materialized temporary with no other owner. A PLACE expression is
        // neither: `b.xs`, `v[i]`, `t.0` and `*p` read storage something else
        // owns, and `compile_expr` hands back that container's own
        // `{ptr, len, cap}` rather than a copy. Tracking it scheduled a
        // `FreeVecBuffer` on a buffer the owner also frees, so `f"{b.xs}"` on a
        // `struct B { xs: Vec[i64] }` was a hard double free, and a
        // `Vec[String]` / nested `Vec` SEGFAULTED because the drain walked
        // elements it did not own. Correct on `--interp` throughout, so this
        // was compiled-only.
        if self.print_vec_operand_is_owned_temp(e) {
            self.track_vec_var(slot, Some(self.llvm_type_for_type_expr(&elem_te)));
        }
        let disp = self.emit_vec_display_fn_te(&elem_te);
        Ok(Some(self.render_via_display_fn(disp, slot)))
    }

    /// B-2026-08-30-39 — the by-pointer display FUNCTION for `Vector[T, N]`,
    /// the sibling of [`Self::emit_tuple_display_fn`] and the half
    /// [`Self::try_compile_vector_display`] could not supply.
    ///
    /// That one renders an f-string PART: it appends into a live accumulator at
    /// the interpolation site, keyed on the expression's span. Every CONTAINER
    /// layer instead needs a `fn(value_ptr, acc)` it can call per element, and
    /// there was none — so a vector inside a `Vec`, a tuple, or any `dbg`
    /// argument fell through `emit_display_fn_for_type_expr` to the by-name
    /// catch-all and PANICKED the compiler with no span, against an interpreter
    /// that rendered all of them.
    ///
    /// SIGNEDNESS COMES FROM THE ELEMENT TYPE HERE, not from the span table the
    /// part renderer consults. An LLVM `iN` is signless, which is why the
    /// expression-keyed path needs `unsigned_vector_exprs` at all; a function
    /// emitted for a `TypeExpr` has the answer in hand and needs no side table.
    /// That also makes this path work where the table has no entry, which is
    /// every nested position — the table records interpolation sites.
    ///
    /// The lane loop is shared with the part renderer
    /// ([`Self::disp_append_vector_lanes`]) precisely so `Vector[u64, 2]` of
    /// `u64::MAX` cannot render `18446744073709551615` at depth 0 and
    /// `-1` one level down.
    pub(super) fn emit_vector_display_fn(
        &mut self,
        elem_te: &TypeExpr,
        lanes: i128,
        vector_te: &TypeExpr,
    ) -> FunctionValue<'ctx> {
        let type_name = Self::display_mangle_te(vector_te);
        let fn_name = format!("{}{type_name}", self.disp_sym_prefix());
        if let Some(f) = self.disp_cache_get(&type_name) {
            return f;
        }
        if let Some(f) = self.module.get_function(&fn_name) {
            self.disp_cache_put(type_name, f);
            return f;
        }

        let vec_llvm_ty = self.llvm_type_for_type_expr(vector_te);
        let unsigned = super::tensor::type_expr_is_unsigned_int(elem_te);
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
        self.disp_cache_put(type_name, display_fn);

        let entry_bb = self.context.append_basic_block(display_fn, "entry");
        self.builder.position_at_end(entry_bb);
        self.current_fn = Some(display_fn);
        let val_ptr = display_fn.get_nth_param(0).unwrap().into_pointer_value();
        let acc = display_fn.get_nth_param(1).unwrap().into_pointer_value();

        // `lanes` is carried for the caller's shape check only; the loop reads
        // the count back off the loaded value, so a disagreement between the
        // TypeExpr's const arg and the lowered `<N x T>` cannot desynchronize
        // the rendering from the memory being read.
        let _ = lanes;
        let loaded = self
            .builder
            .build_load(vec_llvm_ty, val_ptr, "vecfn.val")
            .unwrap();
        if let BasicValueEnum::VectorValue(vv) = loaded {
            // A declined lane kind leaves `Vector(` unterminated, so close the
            // parenthesis either way rather than emitting a half-rendered part.
            match self.disp_append_vector_lanes(acc, vv, unsigned) {
                Ok(true) => {}
                _ => self.disp_append_lit(acc, "Vector(?)"),
            }
        } else {
            self.disp_append_lit(acc, "Vector(?)");
        }

        self.builder.build_return(None).unwrap();

        self.current_fn = saved_fn;
        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        display_fn
    }

    /// B-2026-08-30-39 — append `Vector(l0, l1, …)` for a loaded `<N x T>`.
    ///
    /// Extracted from [`Self::try_compile_vector_display`] so the F-STRING PART
    /// renderer (that one) and the by-pointer display FUNCTION
    /// ([`Self::emit_vector_display_fn`]) share one body. They render the same
    /// value at different depths — `f"{v}"` versus a `v` nested in a `Vec`, a
    /// tuple or a `dbg` — and two copies of this lane loop would be two places
    /// for the lane-signedness rule to drift apart, which is exactly the class
    /// of bug B-2026-08-29-52 filed against the scalar fallback.
    ///
    /// `unsigned` is the caller's to determine, because the two have different
    /// evidence available: the part renderer reads the `unsigned_vector_exprs`
    /// span table (an LLVM `iN` is signless, so the compiled value cannot
    /// answer), while the function knows its element `TypeExpr` outright.
    ///
    /// Returns `false` — having emitted nothing — for a lane type neither
    /// integer nor float. The typechecker restricts `T` to `Numeric`, so this
    /// is unreachable rather than merely rare; both callers decline gracefully
    /// instead of asserting.
    fn disp_append_vector_lanes(
        &mut self,
        acc: PointerValue<'ctx>,
        vv: inkwell::values::VectorValue<'ctx>,
        unsigned: bool,
    ) -> Result<bool, String> {
        let i64_t = self.context.i64_type();
        let i32_t = self.context.i32_type();
        let lanes = vv.get_type().get_size();
        let elem_ty = vv.get_type().get_element_type();
        self.disp_append_lit(acc, "Vector(");
        for i in 0..lanes {
            if i > 0 {
                self.disp_append_lit(acc, ", ");
            }
            let lane = self
                .builder
                .build_extract_element(vv, i32_t.const_int(i as u64, false), "vecd.lane")
                .map_err(|err| format!("vector display extractelement: {err}"))?;
            match elem_ty {
                BasicTypeEnum::IntType(it) => {
                    let iv = lane.into_int_value();
                    let width = it.get_bit_width();
                    if width > 64 {
                        // `%lld`/`%llu` extend to i64 first, which truncates.
                        // Same 128-bit formatter the scalar path uses, so a
                        // lane renders like the bare primitive would.
                        let (bp, ln) = self.format_i128_to_stack_buf(iv, unsigned);
                        self.emit_string_append_raw(acc, bp, ln);
                    } else if width == 1 {
                        // A `Vector[bool, N]` mask. No surface constructs one
                        // today (the comparison methods that would return it
                        // are unimplemented), but rendering it as 1/0 would be
                        // a silent mismatch with the interpreter's `true` /
                        // `false` the day they land, so pick the word here.
                        let t = self
                            .builder
                            .build_global_string_ptr("true", "vecd.t")
                            .unwrap();
                        let f = self
                            .builder
                            .build_global_string_ptr("false", "vecd.f")
                            .unwrap();
                        let sel = self
                            .builder
                            .build_select(
                                iv,
                                t.as_pointer_value(),
                                f.as_pointer_value(),
                                "vecd.bsel",
                            )
                            .unwrap()
                            .into_pointer_value();
                        let len = self
                            .builder
                            .build_select(
                                iv,
                                i64_t.const_int(4, false),
                                i64_t.const_int(5, false),
                                "vecd.blen",
                            )
                            .unwrap()
                            .into_int_value();
                        self.emit_string_append_raw(acc, sel, len);
                    } else if unsigned {
                        let w = self
                            .builder
                            .build_int_z_extend_or_bit_cast(iv, i64_t, "vecd.z")
                            .unwrap();
                        self.disp_append_snprintf(acc, "%llu", w.into());
                    } else {
                        let w = self
                            .builder
                            .build_int_s_extend_or_bit_cast(iv, i64_t, "vecd.s")
                            .unwrap();
                        self.disp_append_snprintf(acc, "%lld", w.into());
                    }
                }
                BasicTypeEnum::FloatType(_) => {
                    // Shortest-round-trip via the runtime formatter, not C
                    // `%g` — the same helper the scalar and struct-field
                    // renderers use, so a lane and a bare float of the same
                    // value cannot print differently. It widens f32 / f16 /
                    // bf16 to f64 itself (bf16 through f32, per B-2026-07-22-1).
                    let (bp, ln) = self.format_f64_to_stack_buf(lane.into_float_value());
                    self.emit_string_append_raw(acc, bp, ln);
                }
                // The typechecker restricts `T` to `Numeric`, so there is no
                // third case; bail rather than assert, and the caller falls
                // through to the paths that ran before this arm existed.
                _ => return Ok(false),
            }
        }

        self.disp_append_lit(acc, ")");
        Ok(true)
    }

    /// B-2026-08-29-52 — render a whole `Vector[T, N]` interpolation part as
    /// `Vector(l0, l1, …)`, matching the interpreter's `Value::Vector` arm.
    ///
    /// Without this arm a vector part fell through to the scalar renderer,
    /// which produced a DIFFERENT wrong answer on each compiled backend: the
    /// JIT printed the aggregate's address (the same number for two vectors of
    /// different element types, which is what identified it as an address),
    /// and AOT printed a single lane read out of the middle of it. Neither
    /// backend nor `karac check` said anything.
    ///
    /// The lane count and element kind come from the compiled `<N x T>` value
    /// itself. SIGNEDNESS cannot: an LLVM `iN` is signless, so `u8` and `i8`
    /// lanes are the same type here. That comes from `unsigned_vector_exprs`,
    /// the span table the SIMD `reduce_min`/`reduce_max` paths already trust
    /// for the same question.
    ///
    /// The decision to take this arm has to be made BEFORE compiling `e` —
    /// once the scalar path has run there is no way back — which is why it
    /// keys on the span table rather than on the compiled value's LLVM type.
    ///
    /// KNOWN RESIDUAL, deliberately not matched: for an unsigned lane holding a
    /// value above `i64::MAX`, this prints the value and the INTERPRETER prints
    /// a signed reinterpretation (`Vector[u64, 2]` of `u64::MAX` renders
    /// `Vector(18446744073709551615, 1)` here and `Vector(-1, 1)` there). The
    /// interpreter's `Value::Vector` holds plain `Value::Int` lanes with no
    /// element type, so its Display cannot recover the signedness its own
    /// INDEXED read gets right (`u[0]` prints the unsigned value). That is a
    /// defect in the reference, filed separately rather than reproduced here —
    /// encoding a known-wrong sign convention into new code to preserve
    /// agreement would make the wrong answer harder to remove later.
    pub(super) fn try_compile_vector_display(
        &mut self,
        e: &Expr,
    ) -> Result<Option<(PointerValue<'ctx>, BasicValueEnum<'ctx>)>, String> {
        let key = (e.span.offset, e.span.length);
        if !self.span_tables.vector_typed_exprs.contains(&key) {
            return Ok(None);
        }
        let val = self.compile_expr(e)?;
        let BasicValueEnum::VectorValue(vv) = val else {
            return Ok(None);
        };
        let unsigned = self.span_tables.unsigned_vector_exprs.contains(&key);

        let vec_ty = self.vec_struct_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();
        let fn_val = self.current_fn.unwrap();
        let acc = self.create_entry_alloca(fn_val, "vecd.acc", vec_ty.into());
        // Entry-block zero AND use-site init, for the reason spelled out on
        // `render_via_display_fn`: the caller registers this accumulator for
        // function-scope cleanup, whose drain runs on paths that never reached
        // the render.
        self.zero_init_str_acc_at_entry(acc);
        let dpp = self
            .builder
            .build_struct_gep(vec_ty, acc, 0, "vecd.dpp")
            .unwrap();
        let lp = self
            .builder
            .build_struct_gep(vec_ty, acc, 1, "vecd.lp")
            .unwrap();
        let cp = self
            .builder
            .build_struct_gep(vec_ty, acc, 2, "vecd.cp")
            .unwrap();
        self.builder.build_store(dpp, ptr_ty.const_null()).unwrap();
        self.builder.build_store(lp, i64_t.const_zero()).unwrap();
        self.builder.build_store(cp, i64_t.const_zero()).unwrap();

        let ok = self.disp_append_vector_lanes(acc, vv, unsigned)?;
        if !ok {
            return Ok(None);
        }

        let out = self.builder.build_load(vec_ty, acc, "vecd.val").unwrap();
        Ok(Some((acc, out)))
    }

    /// B-2026-08-14-31 — the `Map`/`Set` sibling of [`Self::try_compile_vec_display`].
    ///
    /// The identifier arms key off per-variable side tables
    /// (`map_key_type_exprs`, `set_elem_type_exprs`), so a Map or Set reached
    /// any other way — a struct field, a call result, a tuple element, an
    /// element of a `Vec[Map[..]]` — had no entry and fell through to the
    /// value-kind arms. A Map/Set is a single control POINTER, so it printed as
    /// one: `f"{b.m}"` rendered `94259731420368` where the interpreter rendered
    /// `{kk: 1}`, with no diagnostic on either surface and a different address
    /// each run. `compile_print`'s own header comment predicted this ("Map gets
    /// printed as a raw address"); B-2026-07-28-12 closed it for `Vec` and left
    /// the siblings.
    ///
    /// Resolution is the span-keyed `display_map_types` / `display_set_types`,
    /// lowered from `expr_types` exactly as `display_vec_types` is, and the
    /// render goes through the SAME `emit_map_display_fn` / `emit_set_display_fn`
    /// the identifier path uses, so the two spellings cannot disagree.
    ///
    /// A bound collection returns through its own slot and takes no ownership,
    /// mirroring the Vec arm. A materialized temporary IS drop-tracked, under
    /// the same `print_vec_operand_is_owned_temp` gate the Vec arm uses
    /// (B-2026-08-14-36): without it, `println(f"{mk()}")` stranded the whole
    /// handle — control block, bucket storage and every stored key — with no
    /// other owner to free it. The gate is what keeps that from becoming
    /// B-2026-08-14-30's double free: a PLACE expression (`b.m`, `v[i]`, `t.0`)
    /// hands back the container's own handle, not a copy, and freeing it would
    /// crash a program that was merely leaking.
    pub(super) fn try_compile_map_or_set_display(
        &mut self,
        e: &Expr,
    ) -> Result<Option<(PointerValue<'ctx>, BasicValueEnum<'ctx>)>, String> {
        let key = (e.span.offset, e.span.length);
        let map_kv = self.display.display_map_types.get(&key).cloned();
        let set_elem = if map_kv.is_some() {
            None
        } else {
            self.display.display_set_types.get(&key).cloned()
        };
        if map_kv.is_none() && set_elem.is_none() {
            return Ok(None);
        }
        // A bound collection already has a slot and an owner.
        if let ExprKind::Identifier(n) = &e.kind {
            if self.variables.contains_key(n.as_str()) {
                return Ok(None);
            }
        }
        let val = self.compile_expr(e)?;
        // The runtime value is one control pointer; anything else is a shape
        // this arm does not understand, so leave it to the existing paths.
        if !val.is_pointer_value() {
            return Ok(None);
        }
        let fn_val = self.current_fn.unwrap();
        let slot = self.create_entry_alloca(fn_val, "mapset.disp.tmp", val.get_type());
        self.builder.build_store(slot, val).unwrap();
        // B-2026-08-14-36 — queue the handle's scope-exit free when this
        // expression PRODUCED the collection. The per-half drop classification
        // is the binding path's own, reached through `map_cleanup_parts_from_
        // halves` rather than re-derived here: it decides which runtime free the
        // handle gets (plain / drop-vec / per-value drop fn) plus the shared-half
        // rc_dec walks, and a second copy of that reasoning is how a leak turns
        // into a double free.
        if self.print_vec_operand_is_owned_temp(e) {
            let (k_te, v_te) = match (&map_kv, &set_elem) {
                (Some((k, v)), _) => (Some(k.clone()), Some(v.clone())),
                (_, Some(el)) => (Some(el.clone()), None),
                _ => (None, None),
            };
            let (key_is_vec, val_is_vec, key_shared, val_shared, val_drop_fn, key_drop_fn) =
                self.map_cleanup_parts_from_halves(k_te.as_ref(), v_te.as_ref());
            self.track_map_var_with_val_drop(
                slot,
                key_is_vec,
                val_is_vec,
                val_shared,
                key_shared,
                val_drop_fn,
                key_drop_fn,
            );
        }
        // B-2026-08-14-35 — `display_map_types` / `display_set_types` admit the
        // sorted siblings (they share the control-block layout and the same
        // fall-through) but keep only the type arguments; the surface name
        // arrives separately, by span.
        let sorted = self.display.display_sorted_collection_spans.contains(&key);
        let disp = match (&map_kv, &set_elem, sorted) {
            (Some((k, v)), _, true) => self.emit_sorted_map_display_fn(k, v)?,
            (Some((k, v)), _, false) => self.emit_map_display_fn(k, v),
            (_, Some(el), true) => self.emit_sorted_set_display_fn(el)?,
            (_, Some(el), false) => self.emit_set_display_fn(el),
            _ => unreachable!("guarded above"),
        };
        Ok(Some(self.render_via_display_fn(disp, slot)))
    }
}
