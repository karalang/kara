//! Synthesized per-type helper functions: hash, eq, drop, and display.
//!
//! Houses the emit_*_for_type / emit_*_for_type_expr / emit_*_for_tuple
//! family of methods that lazily synthesize per-type LLVM functions
//! for hashing, equality, dropping, and display rendering. These
//! functions are emitted on first demand and cached in the matching
//! `hash_fn_cache` / `eq_fn_cache` / `enum_drop_fns` / `struct_drop_fns`
//! / `display_fn_cache` field on `Codegen`.
//!
//! Includes the FxHash byte-loop primitive `emit_fxhash_over_bytes`
//! consumed by every `emit_hash_fn_*` site, plus the `display_mangle_te`
//! type-name mangler used to key the display cache.

use crate::ast::*;

use inkwell::module::Linkage;
use inkwell::types::{BasicType, BasicTypeEnum};
use inkwell::values::{FunctionValue, IntValue, PointerValue};
use inkwell::AddressSpace;
use inkwell::IntPredicate;

impl<'ctx> super::Codegen<'ctx> {
    // ── Map codegen ───────────────────────────────────────────────

    /// FxHash multiplier — rustc-hash style. Picked by the
    /// `bench/hash_quality/` investigation (2026-05-15) as the
    /// fastest non-cryptographic hash on karac's per-K hash bench
    /// matrix (4-8× faster than FNV-1a on common workloads;
    /// geometric mean 0.56× of FNV-1a baseline across 18 cells).
    /// Mixed via rotate-left-5 + XOR + multiply per chunk.
    const FXHASH_SEED: u64 = 0x517c_c1b7_2722_0a95;
    const FXHASH_ROTATE: u64 = 5;

    /// Emit an FxHash byte loop over `byte_count` bytes starting at
    /// `data_ptr`. Per-byte step is `h = h.rotate_left(5) ^ byte;
    /// h = h * FXHASH_SEED`. Appends basic blocks to `hash_fn_val`.
    /// Builder must be positioned just before the first block of
    /// the loop; on return it is positioned at the exit block.
    /// Returns the accumulated hash `IntValue` (i64).
    ///
    /// For fixed-size `≤8`-byte primitive keys, prefer the inline
    /// fast-path in `emit_hash_fn_for_type` (one zext + one
    /// multiply, no loop) — it produces the same hash output as
    /// this byte loop when the loop runs the same byte count from
    /// an all-zero initial accumulator, because `rotate_left(0, 5)
    /// = 0` and the loop body collapses to `h = byte * SEED` on
    /// iteration 0. Wider primitives and variable-length keys
    /// (Vec, String, Slice) fall through to this byte loop.
    pub(super) fn emit_fxhash_over_bytes(
        &mut self,
        hash_fn_val: FunctionValue<'ctx>,
        data_ptr: PointerValue<'ctx>,
        byte_count: IntValue<'ctx>,
    ) -> IntValue<'ctx> {
        let i64_t = self.context.i64_type();
        let i8_t = self.context.i8_type();
        let seed = i64_t.const_int(Self::FXHASH_SEED, false);
        let rotate_amt = i64_t.const_int(Self::FXHASH_ROTATE, false);
        let rotate_inv = i64_t.const_int(64 - Self::FXHASH_ROTATE, false);

        let pre_bb = self.builder.get_insert_block().unwrap();
        let hdr_bb = self.context.append_basic_block(hash_fn_val, "fx.hdr");
        let bdy_bb = self.context.append_basic_block(hash_fn_val, "fx.bdy");
        let exit_bb = self.context.append_basic_block(hash_fn_val, "fx.exit");

        self.builder.build_unconditional_branch(hdr_bb).unwrap();

        self.builder.position_at_end(hdr_bb);
        let i_phi = self.builder.build_phi(i64_t, "fx.i").unwrap();
        let hash_phi = self.builder.build_phi(i64_t, "fx.hash").unwrap();
        i_phi.add_incoming(&[(&i64_t.const_zero(), pre_bb)]);
        hash_phi.add_incoming(&[(&i64_t.const_zero(), pre_bb)]);
        let i_val = i_phi.as_basic_value().into_int_value();
        let hash_val = hash_phi.as_basic_value().into_int_value();
        let cond = self
            .builder
            .build_int_compare(IntPredicate::ULT, i_val, byte_count, "fx.cond")
            .unwrap();
        self.builder
            .build_conditional_branch(cond, bdy_bb, exit_bb)
            .unwrap();

        self.builder.position_at_end(bdy_bb);
        let byte_ptr = unsafe {
            self.builder
                .build_gep(i8_t, data_ptr, &[i_val], "fx.bp")
                .unwrap()
        };
        let byte = self
            .builder
            .build_load(i8_t, byte_ptr, "fx.b")
            .unwrap()
            .into_int_value();
        let byte64 = self
            .builder
            .build_int_z_extend(byte, i64_t, "fx.b64")
            .unwrap();
        // rotate_left(h, 5) == (h << 5) | (h >> 59)
        let shl = self
            .builder
            .build_left_shift(hash_val, rotate_amt, "fx.shl")
            .unwrap();
        let shr = self
            .builder
            .build_right_shift(hash_val, rotate_inv, false, "fx.shr")
            .unwrap();
        let rotated = self.builder.build_or(shl, shr, "fx.rot").unwrap();
        let xored = self.builder.build_xor(rotated, byte64, "fx.xor").unwrap();
        let new_hash = self.builder.build_int_mul(xored, seed, "fx.mul").unwrap();
        let i_next = self
            .builder
            .build_int_add(i_val, i64_t.const_int(1, false), "fx.i1")
            .unwrap();
        i_phi.add_incoming(&[(&i_next, bdy_bb)]);
        hash_phi.add_incoming(&[(&new_hash, bdy_bb)]);
        self.builder.build_unconditional_branch(hdr_bb).unwrap();

        self.builder.position_at_end(exit_bb);
        hash_val
    }

    /// Emit (or reuse) a module-level `karac_hash_{type_name}(ptr) -> i64` function.
    ///
    /// Per the `bench/hash_quality/` investigation (2026-05-15),
    /// karac's per-K hash is **FxHash** (rustc-hash style
    /// rotate-xor-multiply over 8-byte chunks). Geometric mean
    /// across 18 bench cells: 0.56× of the prior FNV-1a baseline
    /// (1.8× faster overall, up to 4-8× faster on integer keys).
    ///
    /// - **Integer primitives `≤8` bytes** (i8, i16, i32, i64,
    ///   char, bool): inline fast path — load value, zero-extend
    ///   to i64, multiply by `FXHASH_SEED`. One zext + one mul,
    ///   no loop. The initial accumulator is 0, so the per-byte
    ///   shape `h.rotate_left(5) ^ byte; h * SEED` collapses to
    ///   `value * SEED` when processed as a single chunk.
    /// - **`String`**: loads `{ ptr data, i64 len }` from the
    ///   struct and runs the FxHash byte loop over `data[0..len]`.
    /// - **Float primitives** (f32, f64) and **wider integers**
    ///   (i128, u128): byte loop over `sizeof(K)` raw bytes.
    /// - **Structs / other**: byte loop over raw struct bytes
    ///   (correct for value-only structs; tuple combiner in
    ///   `emit_hash_fn_for_tuple` per-field-recurses).
    pub(super) fn emit_hash_fn_for_type(
        &mut self,
        type_name: &str,
        key_ty: BasicTypeEnum<'ctx>,
    ) -> FunctionValue<'ctx> {
        let fn_name = format!("karac_hash_{type_name}");
        if let Some(f) = self.module.get_function(&fn_name) {
            return f;
        }

        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();

        let saved_bb = self.builder.get_insert_block();

        let hash_fn_ty = i64_t.fn_type(&[ptr_ty.into()], false);
        let hash_fn = self
            .module
            .add_function(&fn_name, hash_fn_ty, Some(Linkage::Internal));

        let entry_bb = self.context.append_basic_block(hash_fn, "entry");
        self.builder.position_at_end(entry_bb);
        let key_ptr = hash_fn.get_nth_param(0).unwrap().into_pointer_value();

        if type_name == "String" || type_name == "str" {
            // String struct: { ptr data, i64 len, i64 cap }
            let str_ty = self.vec_struct_type();
            let data_pp = self
                .builder
                .build_struct_gep(str_ty, key_ptr, 0, "s.data.pp")
                .unwrap();
            let data_ptr = self
                .builder
                .build_load(ptr_ty, data_pp, "s.data")
                .unwrap()
                .into_pointer_value();
            let len_p = self
                .builder
                .build_struct_gep(str_ty, key_ptr, 1, "s.len.p")
                .unwrap();
            let len = self
                .builder
                .build_load(i64_t, len_p, "s.len")
                .unwrap()
                .into_int_value();
            let hash = self.emit_fxhash_over_bytes(hash_fn, data_ptr, len);
            self.builder.build_return(Some(&hash)).unwrap();
        } else if let BasicTypeEnum::IntType(int_ty) = key_ty {
            // Integer primitive fast path: load value, zext to
            // i64, multiply by FXHASH_SEED. Matches the byte-loop
            // output for the i==0 case from an all-zero
            // accumulator (rotate(0, 5) = 0 → 0 ^ value = value;
            // value * SEED).
            let bit_width = int_ty.get_bit_width();
            if bit_width <= 64 {
                let raw = self
                    .builder
                    .build_load(int_ty, key_ptr, "fx.prim.raw")
                    .unwrap()
                    .into_int_value();
                let value64 = if bit_width == 64 {
                    raw
                } else {
                    self.builder
                        .build_int_z_extend(raw, i64_t, "fx.prim.zext")
                        .unwrap()
                };
                let seed = i64_t.const_int(Self::FXHASH_SEED, false);
                let hash = self
                    .builder
                    .build_int_mul(value64, seed, "fx.prim.mul")
                    .unwrap();
                self.builder.build_return(Some(&hash)).unwrap();
            } else {
                // Wider integers (i128 / u128): fall back to byte loop.
                let raw_size = key_ty
                    .size_of()
                    .unwrap_or_else(|| i64_t.const_int(8, false));
                let size64 = if raw_size.get_type().get_bit_width() == 64 {
                    raw_size
                } else {
                    self.builder
                        .build_int_z_extend(raw_size, i64_t, "ksz64")
                        .unwrap()
                };
                let hash = self.emit_fxhash_over_bytes(hash_fn, key_ptr, size64);
                self.builder.build_return(Some(&hash)).unwrap();
            }
        } else {
            // Float primitives, structs, other compound types:
            // FxHash byte loop over `sizeof(K)` raw bytes.
            let raw_size = key_ty
                .size_of()
                .unwrap_or_else(|| i64_t.const_int(8, false));
            let size64 = if raw_size.get_type().get_bit_width() == 64 {
                raw_size
            } else {
                self.builder
                    .build_int_z_extend(raw_size, i64_t, "ksz64")
                    .unwrap()
            };
            let hash = self.emit_fxhash_over_bytes(hash_fn, key_ptr, size64);
            self.builder.build_return(Some(&hash)).unwrap();
        }

        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        hash_fn
    }

    /// Emit (or reuse) a module-level `karac_eq_{type_name}(ptr, ptr) -> i1` function.
    ///
    /// - Integer primitives: load both values and `icmp eq`.
    /// - `String`: compare lengths then byte-by-byte.
    /// - Structs/other: byte-by-byte over raw `sizeof(K)` bytes.
    pub(super) fn emit_eq_fn_for_type(
        &mut self,
        type_name: &str,
        key_ty: BasicTypeEnum<'ctx>,
    ) -> FunctionValue<'ctx> {
        let fn_name = format!("karac_eq_{type_name}");
        if let Some(f) = self.module.get_function(&fn_name) {
            return f;
        }

        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();
        let i8_t = self.context.i8_type();
        let bool_t = self.context.bool_type();

        let saved_bb = self.builder.get_insert_block();

        let eq_fn_ty = bool_t.fn_type(&[ptr_ty.into(), ptr_ty.into()], false);
        let eq_fn = self
            .module
            .add_function(&fn_name, eq_fn_ty, Some(Linkage::Internal));

        let entry_bb = self.context.append_basic_block(eq_fn, "entry");
        self.builder.position_at_end(entry_bb);
        let a_ptr = eq_fn.get_nth_param(0).unwrap().into_pointer_value();
        let b_ptr = eq_fn.get_nth_param(1).unwrap().into_pointer_value();

        if type_name == "String" || type_name == "str" {
            // String: compare lengths first, then byte-by-byte on content.
            let str_ty = self.vec_struct_type();
            let la_p = self
                .builder
                .build_struct_gep(str_ty, a_ptr, 1, "la.p")
                .unwrap();
            let lb_p = self
                .builder
                .build_struct_gep(str_ty, b_ptr, 1, "lb.p")
                .unwrap();
            let len_a = self
                .builder
                .build_load(i64_t, la_p, "la")
                .unwrap()
                .into_int_value();
            let len_b = self
                .builder
                .build_load(i64_t, lb_p, "lb")
                .unwrap()
                .into_int_value();

            let neq_bb = self.context.append_basic_block(eq_fn, "neq");
            let bytes_bb = self.context.append_basic_block(eq_fn, "bytes");

            let len_eq = self
                .builder
                .build_int_compare(IntPredicate::EQ, len_a, len_b, "len.eq")
                .unwrap();
            self.builder
                .build_conditional_branch(len_eq, bytes_bb, neq_bb)
                .unwrap();

            // neq_bb: return false
            self.builder.position_at_end(neq_bb);
            self.builder
                .build_return(Some(&bool_t.const_int(0, false)))
                .unwrap();

            // bytes_bb: load data ptrs, enter byte loop
            self.builder.position_at_end(bytes_bb);
            let da_p = self
                .builder
                .build_struct_gep(str_ty, a_ptr, 0, "da.p")
                .unwrap();
            let db_p = self
                .builder
                .build_struct_gep(str_ty, b_ptr, 0, "db.p")
                .unwrap();
            let data_a = self
                .builder
                .build_load(ptr_ty, da_p, "da")
                .unwrap()
                .into_pointer_value();
            let data_b = self
                .builder
                .build_load(ptr_ty, db_p, "db")
                .unwrap()
                .into_pointer_value();

            let loop_hdr = self.context.append_basic_block(eq_fn, "eq.hdr");
            let loop_bdy = self.context.append_basic_block(eq_fn, "eq.bdy");
            let loop_exit = self.context.append_basic_block(eq_fn, "eq.exit");

            self.builder.build_unconditional_branch(loop_hdr).unwrap();

            self.builder.position_at_end(loop_hdr);
            let i_phi = self.builder.build_phi(i64_t, "eq.i").unwrap();
            i_phi.add_incoming(&[(&i64_t.const_zero(), bytes_bb)]);
            let i_val = i_phi.as_basic_value().into_int_value();
            let cond = self
                .builder
                .build_int_compare(IntPredicate::ULT, i_val, len_a, "eq.cond")
                .unwrap();
            self.builder
                .build_conditional_branch(cond, loop_bdy, loop_exit)
                .unwrap();

            self.builder.position_at_end(loop_bdy);
            let bpa = unsafe {
                self.builder
                    .build_gep(i8_t, data_a, &[i_val], "bpa")
                    .unwrap()
            };
            let bpb = unsafe {
                self.builder
                    .build_gep(i8_t, data_b, &[i_val], "bpb")
                    .unwrap()
            };
            let ba = self
                .builder
                .build_load(i8_t, bpa, "ba")
                .unwrap()
                .into_int_value();
            let bb_v = self
                .builder
                .build_load(i8_t, bpb, "bb")
                .unwrap()
                .into_int_value();
            let bytes_eq = self
                .builder
                .build_int_compare(IntPredicate::EQ, ba, bb_v, "beq")
                .unwrap();
            let i_next = self
                .builder
                .build_int_add(i_val, i64_t.const_int(1, false), "eq.i1")
                .unwrap();
            i_phi.add_incoming(&[(&i_next, loop_bdy)]);
            self.builder
                .build_conditional_branch(bytes_eq, loop_hdr, neq_bb)
                .unwrap();

            self.builder.position_at_end(loop_exit);
            self.builder
                .build_return(Some(&bool_t.const_int(1, false)))
                .unwrap();
        } else if let BasicTypeEnum::IntType(int_ty) = key_ty {
            // Integer primitives: load and compare directly.
            let va = self
                .builder
                .build_load(int_ty, a_ptr, "va")
                .unwrap()
                .into_int_value();
            let vb = self
                .builder
                .build_load(int_ty, b_ptr, "vb")
                .unwrap()
                .into_int_value();
            let eq = self
                .builder
                .build_int_compare(IntPredicate::EQ, va, vb, "eq")
                .unwrap();
            self.builder.build_return(Some(&eq)).unwrap();
        } else {
            // Structs and other fixed-size types: byte-by-byte comparison.
            let raw_size = key_ty
                .size_of()
                .unwrap_or_else(|| i64_t.const_int(8, false));
            let size64 = if raw_size.get_type().get_bit_width() == 64 {
                raw_size
            } else {
                self.builder
                    .build_int_z_extend(raw_size, i64_t, "ksz64")
                    .unwrap()
            };

            let neq_bb = self.context.append_basic_block(eq_fn, "neq");
            let loop_hdr = self.context.append_basic_block(eq_fn, "eq.hdr");
            let loop_bdy = self.context.append_basic_block(eq_fn, "eq.bdy");
            let loop_exit = self.context.append_basic_block(eq_fn, "eq.exit");

            self.builder.build_unconditional_branch(loop_hdr).unwrap();

            self.builder.position_at_end(neq_bb);
            self.builder
                .build_return(Some(&bool_t.const_int(0, false)))
                .unwrap();

            self.builder.position_at_end(loop_hdr);
            let i_phi = self.builder.build_phi(i64_t, "eq.i").unwrap();
            i_phi.add_incoming(&[(&i64_t.const_zero(), entry_bb)]);
            let i_val = i_phi.as_basic_value().into_int_value();
            let cond = self
                .builder
                .build_int_compare(IntPredicate::ULT, i_val, size64, "eq.cond")
                .unwrap();
            self.builder
                .build_conditional_branch(cond, loop_bdy, loop_exit)
                .unwrap();

            self.builder.position_at_end(loop_bdy);
            let bpa = unsafe {
                self.builder
                    .build_gep(i8_t, a_ptr, &[i_val], "bpa")
                    .unwrap()
            };
            let bpb = unsafe {
                self.builder
                    .build_gep(i8_t, b_ptr, &[i_val], "bpb")
                    .unwrap()
            };
            let ba = self
                .builder
                .build_load(i8_t, bpa, "ba")
                .unwrap()
                .into_int_value();
            let bb_v = self
                .builder
                .build_load(i8_t, bpb, "bb")
                .unwrap()
                .into_int_value();
            let bytes_eq = self
                .builder
                .build_int_compare(IntPredicate::EQ, ba, bb_v, "beq")
                .unwrap();
            let i_next = self
                .builder
                .build_int_add(i_val, i64_t.const_int(1, false), "eq.i1")
                .unwrap();
            i_phi.add_incoming(&[(&i_next, loop_bdy)]);
            self.builder
                .build_conditional_branch(bytes_eq, loop_hdr, neq_bb)
                .unwrap();

            self.builder.position_at_end(loop_exit);
            self.builder
                .build_return(Some(&bool_t.const_int(1, false)))
                .unwrap();
        }

        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        eq_fn
    }

    pub(super) fn emit_hash_fn_for_type_expr(&mut self, te: &TypeExpr) -> FunctionValue<'ctx> {
        let type_name = Self::mangled_type_name(te);
        let fn_name = format!("karac_hash_{type_name}");
        if let Some(f) = self.module.get_function(&fn_name) {
            return f;
        }
        match &te.kind {
            TypeKind::Tuple(elems) if !elems.is_empty() => {
                self.emit_hash_fn_for_tuple(&type_name, elems)
            }
            // User-struct path: dispatch to per-field hash (mirrors the
            // tuple shape) when the path resolves to a registered
            // struct. The byte-loop fallback in `emit_hash_fn_for_type`
            // hashes raw struct bytes — which includes ptr fields of
            // any `String` / `Vec` / `Map` field — so two structurally-
            // equal instances with different inner allocations hash
            // unequally. AOT used to mask this via the post-codegen
            // `ConstantMerge` pass folding identical string-literal
            // globals into one (so all `"alice"` Tags happened to
            // share a data pointer); LLJIT runs the pre-O2 IR and gets
            // bitten. See `wip-always-jit.md` W3.5 bug 4.
            TypeKind::Path(p)
                if p.segments.len() == 1
                    && self.struct_field_type_exprs.contains_key(&p.segments[0])
                    && !self.shared_types.contains_key(&p.segments[0]) =>
            {
                let struct_name = p.segments[0].clone();
                self.emit_hash_fn_for_struct(&struct_name)
            }
            _ => {
                let key_ty = self.llvm_type_for_type_expr(te);
                self.emit_hash_fn_for_type(&type_name, key_ty)
            }
        }
    }

    /// TypeExpr-aware eq-fn wrapper. Mirror of `emit_hash_fn_for_type_expr`.
    pub(super) fn emit_eq_fn_for_type_expr(&mut self, te: &TypeExpr) -> FunctionValue<'ctx> {
        let type_name = Self::mangled_type_name(te);
        let fn_name = format!("karac_eq_{type_name}");
        if let Some(f) = self.module.get_function(&fn_name) {
            return f;
        }
        match &te.kind {
            TypeKind::Tuple(elems) if !elems.is_empty() => {
                self.emit_eq_fn_for_tuple(&type_name, elems)
            }
            TypeKind::Path(p)
                if p.segments.len() == 1
                    && self.struct_field_type_exprs.contains_key(&p.segments[0])
                    && !self.shared_types.contains_key(&p.segments[0]) =>
            {
                let struct_name = p.segments[0].clone();
                self.emit_eq_fn_for_struct(&struct_name)
            }
            _ => {
                let key_ty = self.llvm_type_for_type_expr(te);
                self.emit_eq_fn_for_type(&type_name, key_ty)
            }
        }
    }

    /// Per-field-recursive hash for a registered user struct. Uses the
    /// struct's LLVM type from `self.struct_types` and the field
    /// TypeExprs cached during `declare_structs` in
    /// `self.struct_field_type_exprs`. Shape mirrors
    /// `emit_hash_fn_for_tuple`.
    ///
    /// Only invoked for non-shared structs (value layout): shared
    /// structs flow through a different code path that's pointer-
    /// based already (the heap layout has a refcount prefix; identity
    /// equality / refcount hashing applies). Map-of-shared-struct keys
    /// route through `emit_hash_fn_for_type`'s integer/pointer path,
    /// not here.
    pub(super) fn emit_hash_fn_for_struct(&mut self, struct_name: &str) -> FunctionValue<'ctx> {
        let fn_name = format!("karac_hash_{struct_name}");
        if let Some(f) = self.module.get_function(&fn_name) {
            return f;
        }
        let field_tes = self
            .struct_field_type_exprs
            .get(struct_name)
            .cloned()
            .expect("emit_hash_fn_for_struct: struct must be registered");
        let struct_ty = *self
            .struct_types
            .get(struct_name)
            .expect("emit_hash_fn_for_struct: struct LLVM type must be registered");
        let child_fns: Vec<FunctionValue<'ctx>> = field_tes
            .iter()
            .map(|te| self.emit_hash_fn_for_type_expr(te))
            .collect();

        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();

        let saved_bb = self.builder.get_insert_block();
        let hash_fn_ty = i64_t.fn_type(&[ptr_ty.into()], false);
        let hash_fn = self
            .module
            .add_function(&fn_name, hash_fn_ty, Some(Linkage::Internal));

        let entry_bb = self.context.append_basic_block(hash_fn, "entry");
        self.builder.position_at_end(entry_bb);
        let key_ptr = hash_fn.get_nth_param(0).unwrap().into_pointer_value();

        // FxHash tail-mix, identical to the tuple combiner.
        let seed = i64_t.const_int(Self::FXHASH_SEED, false);
        let rotate_amt = i64_t.const_int(Self::FXHASH_ROTATE, false);
        let rotate_inv = i64_t.const_int(64 - Self::FXHASH_ROTATE, false);
        let mut state: IntValue<'ctx> = i64_t.const_zero();
        for (i, child_fn) in child_fns.iter().enumerate() {
            let field_ptr = self
                .builder
                .build_struct_gep(struct_ty, key_ptr, i as u32, &format!("s.f{i}.p"))
                .unwrap();
            let elem_hash = self
                .builder
                .build_call(*child_fn, &[field_ptr.into()], &format!("s.f{i}.h"))
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_int_value();
            let shl = self
                .builder
                .build_left_shift(state, rotate_amt, &format!("s.f{i}.shl"))
                .unwrap();
            let shr = self
                .builder
                .build_right_shift(state, rotate_inv, false, &format!("s.f{i}.shr"))
                .unwrap();
            let rotated = self
                .builder
                .build_or(shl, shr, &format!("s.f{i}.rot"))
                .unwrap();
            let xored = self
                .builder
                .build_xor(rotated, elem_hash, &format!("s.f{i}.xor"))
                .unwrap();
            state = self
                .builder
                .build_int_mul(xored, seed, &format!("s.f{i}.mul"))
                .unwrap();
        }
        self.builder.build_return(Some(&state)).unwrap();

        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        hash_fn
    }

    /// Per-field-recursive eq for a registered user struct. Mirrors
    /// `emit_eq_fn_for_tuple`; short-circuits to `false` on the first
    /// mismatching field.
    pub(super) fn emit_eq_fn_for_struct(&mut self, struct_name: &str) -> FunctionValue<'ctx> {
        let fn_name = format!("karac_eq_{struct_name}");
        if let Some(f) = self.module.get_function(&fn_name) {
            return f;
        }
        let field_tes = self
            .struct_field_type_exprs
            .get(struct_name)
            .cloned()
            .expect("emit_eq_fn_for_struct: struct must be registered");
        let struct_ty = *self
            .struct_types
            .get(struct_name)
            .expect("emit_eq_fn_for_struct: struct LLVM type must be registered");
        let child_fns: Vec<FunctionValue<'ctx>> = field_tes
            .iter()
            .map(|te| self.emit_eq_fn_for_type_expr(te))
            .collect();

        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let bool_t = self.context.bool_type();

        let saved_bb = self.builder.get_insert_block();
        let eq_fn_ty = bool_t.fn_type(&[ptr_ty.into(), ptr_ty.into()], false);
        let eq_fn = self
            .module
            .add_function(&fn_name, eq_fn_ty, Some(Linkage::Internal));

        let entry_bb = self.context.append_basic_block(eq_fn, "entry");
        let neq_bb = self.context.append_basic_block(eq_fn, "neq");
        self.builder.position_at_end(neq_bb);
        self.builder
            .build_return(Some(&bool_t.const_int(0, false)))
            .unwrap();

        self.builder.position_at_end(entry_bb);
        let a_ptr = eq_fn.get_nth_param(0).unwrap().into_pointer_value();
        let b_ptr = eq_fn.get_nth_param(1).unwrap().into_pointer_value();

        for (i, child_fn) in child_fns.iter().enumerate() {
            let fa = self
                .builder
                .build_struct_gep(struct_ty, a_ptr, i as u32, &format!("s.fa{i}"))
                .unwrap();
            let fb = self
                .builder
                .build_struct_gep(struct_ty, b_ptr, i as u32, &format!("s.fb{i}"))
                .unwrap();
            let r = self
                .builder
                .build_call(*child_fn, &[fa.into(), fb.into()], &format!("s.eq{i}"))
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_int_value();
            let next_bb = self
                .context
                .append_basic_block(eq_fn, &format!("eq.next{i}"));
            self.builder
                .build_conditional_branch(r, next_bb, neq_bb)
                .unwrap();
            self.builder.position_at_end(next_bb);
        }
        self.builder
            .build_return(Some(&bool_t.const_int(1, false)))
            .unwrap();

        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        eq_fn
    }

    /// Emit a per-field-recursive hash function for an n-tuple. Each field's
    /// hash is computed by recursing into `emit_hash_fn_for_type_expr` (so
    /// `(String, i64)` correctly hashes the String contents, not the struct
    /// bytes), then combined into a running state via the FxHash tail-mix
    /// `state = (state.rotate_left(5) ^ field_hash) * FXHASH_SEED`. Matches
    /// the per-K hash emission shape selected by the
    /// `bench/hash_quality/` investigation.
    pub(super) fn emit_hash_fn_for_tuple(
        &mut self,
        type_name: &str,
        elems: &[TypeExpr],
    ) -> FunctionValue<'ctx> {
        let fn_name = format!("karac_hash_{type_name}");
        if let Some(f) = self.module.get_function(&fn_name) {
            return f;
        }
        let elems_owned: Vec<TypeExpr> = elems.to_vec();
        let child_fns: Vec<FunctionValue<'ctx>> = elems_owned
            .iter()
            .map(|e| self.emit_hash_fn_for_type_expr(e))
            .collect();
        let field_tys: Vec<BasicTypeEnum<'ctx>> = elems_owned
            .iter()
            .map(|e| self.llvm_type_for_type_expr(e))
            .collect();
        let tuple_ty = self.context.struct_type(&field_tys, false);

        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();

        let saved_bb = self.builder.get_insert_block();
        let hash_fn_ty = i64_t.fn_type(&[ptr_ty.into()], false);
        let hash_fn = self
            .module
            .add_function(&fn_name, hash_fn_ty, Some(Linkage::Internal));

        let entry_bb = self.context.append_basic_block(hash_fn, "entry");
        self.builder.position_at_end(entry_bb);
        let key_ptr = hash_fn.get_nth_param(0).unwrap().into_pointer_value();

        // FxHash tail-mix: state = (state.rotate_left(5) ^
        // field_hash) * FXHASH_SEED. Initial state = 0 collapses
        // the first field's mix to `field_hash_0 * SEED`,
        // matching the inline primitive fast path for a 1-element
        // "tuple". For n>1 fields, subsequent fields rotate and
        // chain.
        let seed = i64_t.const_int(Self::FXHASH_SEED, false);
        let rotate_amt = i64_t.const_int(Self::FXHASH_ROTATE, false);
        let rotate_inv = i64_t.const_int(64 - Self::FXHASH_ROTATE, false);
        let mut state: IntValue<'ctx> = i64_t.const_zero();
        for (i, child_fn) in child_fns.iter().enumerate() {
            let field_ptr = self
                .builder
                .build_struct_gep(tuple_ty, key_ptr, i as u32, &format!("t.f{i}.p"))
                .unwrap();
            let elem_hash = self
                .builder
                .build_call(*child_fn, &[field_ptr.into()], &format!("t.f{i}.h"))
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_int_value();
            let shl = self
                .builder
                .build_left_shift(state, rotate_amt, &format!("t.f{i}.shl"))
                .unwrap();
            let shr = self
                .builder
                .build_right_shift(state, rotate_inv, false, &format!("t.f{i}.shr"))
                .unwrap();
            let rotated = self
                .builder
                .build_or(shl, shr, &format!("t.f{i}.rot"))
                .unwrap();
            let xored = self
                .builder
                .build_xor(rotated, elem_hash, &format!("t.f{i}.xor"))
                .unwrap();
            state = self
                .builder
                .build_int_mul(xored, seed, &format!("t.f{i}.mul"))
                .unwrap();
        }
        self.builder.build_return(Some(&state)).unwrap();

        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        hash_fn
    }

    /// Emit a per-field-recursive eq function for an n-tuple. Each field is
    /// compared via the recursively-emitted per-field eq fn; the function
    /// short-circuits to `false` on the first mismatch.
    pub(super) fn emit_eq_fn_for_tuple(
        &mut self,
        type_name: &str,
        elems: &[TypeExpr],
    ) -> FunctionValue<'ctx> {
        let fn_name = format!("karac_eq_{type_name}");
        if let Some(f) = self.module.get_function(&fn_name) {
            return f;
        }
        let elems_owned: Vec<TypeExpr> = elems.to_vec();
        let child_fns: Vec<FunctionValue<'ctx>> = elems_owned
            .iter()
            .map(|e| self.emit_eq_fn_for_type_expr(e))
            .collect();
        let field_tys: Vec<BasicTypeEnum<'ctx>> = elems_owned
            .iter()
            .map(|e| self.llvm_type_for_type_expr(e))
            .collect();
        let tuple_ty = self.context.struct_type(&field_tys, false);

        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let bool_t = self.context.bool_type();

        let saved_bb = self.builder.get_insert_block();
        let eq_fn_ty = bool_t.fn_type(&[ptr_ty.into(), ptr_ty.into()], false);
        let eq_fn = self
            .module
            .add_function(&fn_name, eq_fn_ty, Some(Linkage::Internal));

        let entry_bb = self.context.append_basic_block(eq_fn, "entry");
        let neq_bb = self.context.append_basic_block(eq_fn, "neq");
        self.builder.position_at_end(neq_bb);
        self.builder
            .build_return(Some(&bool_t.const_int(0, false)))
            .unwrap();

        self.builder.position_at_end(entry_bb);
        let a_ptr = eq_fn.get_nth_param(0).unwrap().into_pointer_value();
        let b_ptr = eq_fn.get_nth_param(1).unwrap().into_pointer_value();

        for (i, child_fn) in child_fns.iter().enumerate() {
            let fa = self
                .builder
                .build_struct_gep(tuple_ty, a_ptr, i as u32, &format!("t.fa{i}"))
                .unwrap();
            let fb = self
                .builder
                .build_struct_gep(tuple_ty, b_ptr, i as u32, &format!("t.fb{i}"))
                .unwrap();
            let r = self
                .builder
                .build_call(*child_fn, &[fa.into(), fb.into()], &format!("t.eq{i}"))
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_int_value();
            let next_bb = self
                .context
                .append_basic_block(eq_fn, &format!("eq.next{i}"));
            self.builder
                .build_conditional_branch(r, next_bb, neq_bb)
                .unwrap();
            self.builder.position_at_end(next_bb);
        }
        self.builder
            .build_return(Some(&bool_t.const_int(1, false)))
            .unwrap();

        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        eq_fn
    }
}
