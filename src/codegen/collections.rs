//! Set method dispatch, array/repeat literals, and indexing codegen.
//!
//! Houses Set method dispatch (`compile_set_method`, `emit_set_op_iter`
//! for union/intersection/difference), array literal lowering
//! (`compile_array_literal`, `try_emit_zero_init_array_let`,
//! `compile_repeat_literal`), and the indexing family
//! (`compile_index`, `compile_vec_index`, `index_bounds_already_proven`,
//! `emit_split_bounds_check`, `compile_vec_index_store`,
//! `compile_slice_index_store`, `compile_slice_index`,
//! `compile_index_store`).

use crate::ast::*;

use inkwell::types::{BasicType, BasicTypeEnum, StructType};
use inkwell::values::{BasicValueEnum, PointerValue};
use inkwell::AddressSpace;

use super::helpers::vec_inner_type_expr;
use super::state::{AssertedIndexBound, SetOpFilter, SoaGroup, VarSlot};

impl<'ctx> super::Codegen<'ctx> {
    /// Compile method calls on `Set[T]` variables. `Set[T]` lowers to
    /// `Map[T, ()]` at codegen so all Map runtime fns are reused; the
    /// value-side allocas are sized to the (zero-byte) unit type and the
    /// runtime's `(key_size + val_size).max(1)` makes the value-store a
    /// no-op.
    ///
    /// Handled methods: `len`, `is_empty`, `insert`, `contains`, `remove`,
    /// `clear`. `union` / `intersection` / `difference` are out-of-line in
    /// `compile_set_op_method` so this fn stays focused on the runtime-
    /// passthrough cases.
    pub(super) fn compile_set_method(
        &mut self,
        var_name: &str,
        method: &str,
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();
        let bool_t = self.context.bool_type();
        let i8_t = self.context.i8_type();

        self.variables
            .get(var_name)
            .copied()
            .ok_or_else(|| format!("unknown set variable '{var_name}'"))?;
        // Use `get_data_ptr` so `mut ref Set[T]` params unwrap one
        // ref-level before the handle load. Owned bindings yield
        // `slot.ptr` directly. Mirrors the Map-side fix.
        let handle_ptr = self
            .get_data_ptr(var_name)
            .ok_or_else(|| format!("unknown set variable '{var_name}'"))?;
        let set_handle = self
            .builder
            .build_load(ptr_ty, handle_ptr, "set.handle")
            .unwrap()
            .into_pointer_value();

        let elem_ty = self
            .set_elem_types
            .get(var_name)
            .copied()
            .unwrap_or(i64_t.into());

        match method {
            "len" => {
                let len = self
                    .builder
                    .build_call(self.karac_map_len_fn, &[set_handle.into()], "set.len")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic();
                Ok(len)
            }
            "is_empty" => {
                let len = self
                    .builder
                    .build_call(self.karac_map_len_fn, &[set_handle.into()], "set.len")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_int_value();
                let zero = i64_t.const_int(0, false);
                let cmp = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::EQ, len, zero, "set.is_empty")
                    .unwrap();
                Ok(cmp.into())
            }
            "insert" => {
                if args.is_empty() {
                    return Err("Set.insert requires a value argument".to_string());
                }
                let elem_val = self.compile_expr(&args[0].value)?;
                // Consume-site ownership pair, identical to `Vec.push` /
                // `Map.insert`: an f-string element (`s.insert(f"…")`) moves
                // its buffer in — disarm the staged accumulator's scope-exit
                // free; an owned String/Vec PARAM element deep-copies — the
                // Set takes ownership of a private copy while the caller
                // retains the original buffer's free under the by-value
                // header ABI (kata-22 owned-param UAF family).
                self.suppress_fstr_acc_if_moved_out(&args[0].value);
                let elem_val = self.maybe_defensive_copy_param_arg(&args[0].value, elem_val);
                // Move semantics for tracked Vec/String elements: the
                // bucket bit-copies the element's `{ptr, len, cap}` and
                // the `karac_map_free_with_drop_vec` cleanup (when
                // `key_is_vec = true` for `Set[Vec[T]]` / `Set[String]`)
                // would double-free against the source binding's own
                // scope-exit `FreeVecBuffer`. Suppress so the Set
                // becomes the unique owner. Mirrors the `Map.insert`
                // key-side suppression added alongside the recursive
                // key-drop path.
                self.suppress_source_vec_cleanup_for_arg(&args[0].value);
                let fn_val = self.current_fn.unwrap();
                let elem_slot = self.create_entry_alloca(fn_val, "set.insert.elem", elem_ty);
                self.builder.build_store(elem_slot, elem_val).unwrap();
                // val_size = 0, so dummy_unit / dummy_out can be a single
                // shared i8 alloca — the runtime store-of-zero-bytes is a
                // no-op regardless of the byte's contents.
                let dummy = self.create_entry_alloca(fn_val, "set.dummy", i8_t.into());
                let existed = self
                    .builder
                    .build_call(
                        self.karac_map_insert_old_fn,
                        &[
                            set_handle.into(),
                            elem_slot.into(),
                            dummy.into(),
                            dummy.into(),
                        ],
                        "set.insert.existed",
                    )
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_int_value();
                // Set.insert returns true when the value was newly inserted
                // (matches Rust HashSet::insert), so flip `existed`.
                let one = bool_t.const_int(1, false);
                let inserted = self
                    .builder
                    .build_xor(existed, one, "set.insert.inserted")
                    .unwrap();
                Ok(inserted.into())
            }
            "contains" => {
                if args.is_empty() {
                    return Err("Set.contains requires a value argument".to_string());
                }
                let elem_val = self.compile_expr(&args[0].value)?;
                let fn_val = self.current_fn.unwrap();
                let elem_slot = self.create_entry_alloca(fn_val, "set.contains.elem", elem_ty);
                self.builder.build_store(elem_slot, elem_val).unwrap();
                let found = self
                    .builder
                    .build_call(
                        self.karac_map_contains_fn,
                        &[set_handle.into(), elem_slot.into()],
                        "set.contains",
                    )
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic();
                Ok(found)
            }
            "remove" => {
                if args.is_empty() {
                    return Err("Set.remove requires a value argument".to_string());
                }
                let elem_val = self.compile_expr(&args[0].value)?;
                let fn_val = self.current_fn.unwrap();
                let elem_slot = self.create_entry_alloca(fn_val, "set.remove.elem", elem_ty);
                self.builder.build_store(elem_slot, elem_val).unwrap();
                // val_size = 0 → dummy out slot is shared; contents irrelevant.
                let dummy = self.create_entry_alloca(fn_val, "set.dummy", i8_t.into());
                let existed = self
                    .builder
                    .build_call(
                        self.karac_map_remove_old_fn,
                        &[set_handle.into(), elem_slot.into(), dummy.into()],
                        "set.remove.existed",
                    )
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic();
                Ok(existed)
            }
            "clear" => {
                self.builder
                    .build_call(self.karac_map_clear_fn, &[set_handle.into()], "")
                    .unwrap();
                Ok(i64_t.const_int(0, false).into())
            }
            "union" | "intersection" | "difference" => {
                if args.is_empty() {
                    return Err(format!("Set.{method} requires another set as argument"));
                }
                let other_handle = self.compile_expr(&args[0].value)?.into_pointer_value();
                // Element TypeExpr drives clone/hash/eq fn synthesis. Without
                // it we can't deep-clone non-Copy elements (String, …) safely.
                let elem_te = self
                    .set_elem_type_exprs
                    .get(var_name)
                    .cloned()
                    .ok_or_else(|| {
                        format!("codegen: Set.{method} missing elem TypeExpr for '{var_name}'")
                    })?;

                let elem_size = elem_ty
                    .size_of()
                    .unwrap_or_else(|| i64_t.const_int(8, false));
                let val_size = i64_t.const_int(0, false);
                let hash_fn = self.emit_hash_fn_for_type_expr(&elem_te);
                let eq_fn = self.emit_eq_fn_for_type_expr(&elem_te);

                let new_handle = self
                    .builder
                    .build_call(
                        self.karac_map_new_fn,
                        &[
                            elem_size.into(),
                            val_size.into(),
                            hash_fn.as_global_value().as_pointer_value().into(),
                            eq_fn.as_global_value().as_pointer_value().into(),
                        ],
                        "set.op.new",
                    )
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_pointer_value();

                match method {
                    "union" => {
                        // Clone all of self → dst (dst empty, no duplicates),
                        // then iterate other and insert clones for elements
                        // not already in self. The "skip if in self" check
                        // (rather than "skip if in dst") avoids a probe into
                        // the partially-built dst.
                        self.emit_set_op_iter(
                            set_handle,
                            new_handle,
                            SetOpFilter::Always,
                            &elem_te,
                        );
                        self.emit_set_op_iter(
                            other_handle,
                            new_handle,
                            SetOpFilter::NotContainsIn(set_handle),
                            &elem_te,
                        );
                    }
                    "intersection" => {
                        self.emit_set_op_iter(
                            set_handle,
                            new_handle,
                            SetOpFilter::ContainsIn(other_handle),
                            &elem_te,
                        );
                    }
                    "difference" => {
                        self.emit_set_op_iter(
                            set_handle,
                            new_handle,
                            SetOpFilter::NotContainsIn(other_handle),
                            &elem_te,
                        );
                    }
                    _ => unreachable!(),
                }
                Ok(new_handle.into())
            }
            _ => Err(format!("codegen: Set.{method} not yet implemented")),
        }
    }

    /// Iterate `src_handle`, optionally filter elements through `mode`,
    /// per-element-clone the survivors and insert them into `dst_handle`.
    /// Used by `Set.union` / `intersection` / `difference` codegen — each
    /// op materialises a fresh empty `dst_handle` and calls this once
    /// (intersection / difference) or twice (union: once unfiltered from
    /// `self`, once filtered against `self` from `other`).
    ///
    /// The "skip" branch jumps back to the iterator header, preserving the
    /// invariant that `karac_map_iter_free` runs exactly once per call —
    /// at the exit block, only after `karac_map_iter_next` returned false.
    /// Element clones for skipped survivors never happen, so there is no
    /// leak even when the per-element clone allocates (e.g. `String`).
    pub(super) fn emit_set_op_iter(
        &mut self,
        src_handle: PointerValue<'ctx>,
        dst_handle: PointerValue<'ctx>,
        mode: SetOpFilter<'ctx>,
        elem_te: &TypeExpr,
    ) {
        let i8_t = self.context.i8_type();
        let elem_ty = self.llvm_type_for_type_expr(elem_te);
        let clone_fn = self.emit_clone_fn_for_type_expr(elem_te);
        let fn_val = self.current_fn.unwrap();

        let elem_out = self.create_entry_alloca(fn_val, "setop.k.out", elem_ty);
        let clone_slot = self.create_entry_alloca(fn_val, "setop.k.clone", elem_ty);
        let dummy = self.create_entry_alloca(fn_val, "setop.dummy", i8_t.into());

        let iter_handle = self
            .builder
            .build_call(
                self.karac_map_iter_new_fn,
                &[src_handle.into()],
                "setop.iter",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();

        let hdr_bb = self.context.append_basic_block(fn_val, "setop.iter.hdr");
        let bdy_bb = self.context.append_basic_block(fn_val, "setop.iter.bdy");
        let exit_bb = self.context.append_basic_block(fn_val, "setop.iter.exit");
        self.builder.build_unconditional_branch(hdr_bb).unwrap();

        self.builder.position_at_end(hdr_bb);
        let has = self
            .builder
            .build_call(
                self.karac_map_iter_next_fn,
                &[iter_handle.into(), elem_out.into(), dummy.into()],
                "setop.iter.has",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        self.builder
            .build_conditional_branch(has, bdy_bb, exit_bb)
            .unwrap();

        self.builder.position_at_end(bdy_bb);
        match mode {
            SetOpFilter::Always => {}
            SetOpFilter::ContainsIn(other) | SetOpFilter::NotContainsIn(other) => {
                let pass_bb = self.context.append_basic_block(fn_val, "setop.iter.pass");
                let found = self
                    .builder
                    .build_call(
                        self.karac_map_contains_fn,
                        &[other.into(), elem_out.into()],
                        "setop.contains",
                    )
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_int_value();
                let cond = match mode {
                    SetOpFilter::ContainsIn(_) => found,
                    SetOpFilter::NotContainsIn(_) => self
                        .builder
                        .build_xor(
                            found,
                            self.context.bool_type().const_int(1, false),
                            "setop.neg",
                        )
                        .unwrap(),
                    SetOpFilter::Always => unreachable!(),
                };
                self.builder
                    .build_conditional_branch(cond, pass_bb, hdr_bb)
                    .unwrap();
                self.builder.position_at_end(pass_bb);
            }
        }
        self.builder
            .build_call(clone_fn, &[elem_out.into(), clone_slot.into()], "")
            .unwrap();
        self.builder
            .build_call(
                self.karac_map_insert_fn(),
                &[dst_handle.into(), clone_slot.into(), dummy.into()],
                "",
            )
            .unwrap();
        self.builder.build_unconditional_branch(hdr_bb).unwrap();

        self.builder.position_at_end(exit_bb);
        self.builder
            .build_call(self.karac_map_iter_free_fn, &[iter_handle.into()], "")
            .unwrap();
    }

    pub(super) fn compile_array_literal(
        &mut self,
        elems: &[Expr],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        if elems.is_empty() {
            return Ok(self.context.i64_type().const_int(0, false).into());
        }
        let vals: Vec<BasicValueEnum<'ctx>> = elems
            .iter()
            .map(|e| self.compile_expr(e))
            .collect::<Result<_, _>>()?;
        let elem_ty = vals[0].get_type();
        let arr_ty = elem_ty.array_type(vals.len() as u32);
        let mut agg = arr_ty.get_undef();
        for (idx, val) in vals.iter().enumerate() {
            agg = self
                .builder
                .build_insert_value(agg, *val, idx as u32, "arr.elem")
                .unwrap()
                .into_array_value();
        }
        Ok(agg.into())
    }

    /// Compile a `Vec[a, b, c]` prefix literal at expression position.
    /// Empty `Vec[]` returns the canonical `{null, 0, 0}` aggregate
    /// (matches the `Vec.new()` shape and lets the typechecker's
    /// expected-type carrier supply T). Non-empty: malloc a buffer of
    /// `items.len() * sizeof(elem)`, store each compiled item into its
    /// slot, return `{buf, len, len}` (cap equals len at construction —
    /// subsequent pushes trigger grow when the (n+1)-th lands). The
    /// element LLVM type is recovered from the first compiled item;
    /// downstream `.push` / `.len` / `.remove` etc. all dispatch
    /// through the same `{ptr, len, cap}` shape `Vec.new()` /
    /// `Vec.with_capacity` produce.
    ///
    /// Surfaced as a v1 codegen gap by the backend TODO API kata
    /// Slice 4: `compile_expr` had no `ExprKind::PrefixCollectionLiteral`
    /// arm, so `Json.Array(Vec[a, b])` and even `let xs: Vec[i64] =
    /// Vec[1, 2, 3];` fell through to `i64 0` at exprs.rs:345.
    pub(super) fn compile_vec_prefix_literal(
        &mut self,
        items: &[Expr],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let i64_t = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let vec_ty = self.vec_struct_type();

        if items.is_empty() {
            // Empty literal → `{null, 0, 0}` aggregate. Matches the
            // runtime invariant the `Vec.new()` arm produces (and which
            // the slice-a Vec.new() module-init codegen at `d92f3da`
            // emits as `zeroinitializer`). No heap allocation needed.
            let null_ptr = ptr_ty.const_null();
            let zero = i64_t.const_int(0, false);
            let mut agg = vec_ty.get_undef();
            agg = self
                .builder
                .build_insert_value(agg, null_ptr, 0, "vec.lit.empty.data")
                .unwrap()
                .into_struct_value();
            agg = self
                .builder
                .build_insert_value(agg, zero, 1, "vec.lit.empty.len")
                .unwrap()
                .into_struct_value();
            agg = self
                .builder
                .build_insert_value(agg, zero, 2, "vec.lit.empty.cap")
                .unwrap()
                .into_struct_value();
            return Ok(agg.into());
        }

        // Compile every item up front so we can recover the LLVM
        // element type from the first one. The compiled vals stay in
        // SSA form; we store each into the heap buffer below. (Each
        // compile_expr call may emit side-effecting IR — e.g.
        // `String.clone()` allocates — so the order matters; we
        // preserve source order.)
        let vals: Vec<BasicValueEnum<'ctx>> = items
            .iter()
            .map(|e| self.compile_expr(e))
            .collect::<Result<_, _>>()?;
        let elem_ty = vals[0].get_type();
        let n_const = i64_t.const_int(items.len() as u64, false);

        // malloc(items.len() * sizeof(elem)). LLVM accepts a constant
        // size here so the runtime allocator can fast-path the request,
        // but the IR-shape stays identical to `Vec.with_capacity` /
        // `Vec.filled` for codegen uniformity.
        let elem_size = elem_ty.size_of().unwrap();
        let alloc_bytes = self
            .builder
            .build_int_mul(n_const, elem_size, "vec.lit.alloc_bytes")
            .unwrap();
        let buf = self
            .builder
            .build_call(self.malloc_fn, &[alloc_bytes.into()], "vec.lit.buf")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();

        // Store each item at its source-order slot via GEP. Bit-copy
        // semantics — for aggregate element types (Vec[Vec[T]], etc.)
        // the per-slot store aliases the source's storage. Same
        // limitation `Vec.filled` documents; nested-collection
        // element types route through the existing track_vec_var
        // machinery at the consuming binding site.
        for (i, val) in vals.iter().enumerate() {
            let idx = i64_t.const_int(i as u64, false);
            let elem_ptr = unsafe {
                self.builder
                    .build_gep(elem_ty, buf, &[idx], &format!("vec.lit.elem.{}.ptr", i))
                    .unwrap()
            };
            self.builder.build_store(elem_ptr, *val).unwrap();
        }

        // Build {data=buf, len=n, cap=n} aggregate. Cap equals len so
        // the first subsequent push triggers grow; matches Vec.filled's
        // shape.
        let mut agg = vec_ty.get_undef();
        agg = self
            .builder
            .build_insert_value(agg, buf, 0, "vec.lit.data")
            .unwrap()
            .into_struct_value();
        agg = self
            .builder
            .build_insert_value(agg, n_const, 1, "vec.lit.len")
            .unwrap()
            .into_struct_value();
        agg = self
            .builder
            .build_insert_value(agg, n_const, 2, "vec.lit.cap")
            .unwrap()
            .into_struct_value();
        Ok(agg.into())
    }

    /// Let-binding fast path for `let buf: Array[T, N] = [zero; N]`.
    /// Returns `Some(Ok(()))` on success, `None` if the RHS doesn't match
    /// the literal-zero repeat pattern (caller falls through to the
    /// general `compile_expr` path), or `Some(Err)` on a structural
    /// problem (e.g. unsupported element type).
    ///
    /// Lowers to `alloca [N x T]; call @llvm.memset.*(alloca, 0, N*sizeof(T))`,
    /// bypassing the `store [N x T] zeroinitializer` IR that LLVM's downstream
    /// codegen passes crash on at large N. The memset is what LLVM would emit
    /// for the aggregate store anyway — this just sidesteps the codegen path
    /// that explodes the constant store into per-element machine instructions.
    ///
    /// Matched literal-zero shapes: `Integer(0)`, `Bool(false)`, `Float`
    /// whose IEEE bit pattern is all-zero (`+0.0`, not `-0.0`).
    pub(super) fn try_emit_zero_init_array_let(
        &mut self,
        name: &str,
        value: &Expr,
        ty: Option<&TypeExpr>,
    ) -> Option<Result<(), String>> {
        let ExprKind::RepeatLiteral {
            type_name,
            value: rep_val,
            count,
        } = &value.kind
        else {
            return None;
        };
        // Vec form has its own heap-alloc shape — out of scope.
        if matches!(type_name.as_deref(), Some("Vec")) {
            return None;
        }
        // Literal-zero detection. Floats use bit-pattern equality so `-0.0`
        // doesn't take the path (would lose the sign bit).
        let is_zero_lit = match &rep_val.kind {
            ExprKind::Integer(0, _) => true,
            ExprKind::Bool(false) => true,
            ExprKind::Float(f, _) => f.to_bits() == 0,
            _ => false,
        };
        if !is_zero_lit {
            return None;
        }
        let n = match &count.kind {
            ExprKind::Integer(n, _) if *n > 0 => *n as u32,
            _ => return None,
        };
        // Element LLVM type: from `Array[T, N]` annotation if present, else
        // inferred from the literal's natural type.
        let elem_llvm_ty: BasicTypeEnum<'ctx> = if let Some(te) = ty {
            let TypeKind::Path(path) = &te.kind else {
                return None;
            };
            if path.segments.first().map(|s| s.as_str()) != Some("Array") {
                return None;
            }
            let args = path.generic_args.as_ref()?;
            if args.len() != 2 {
                return None;
            }
            match &args[0] {
                GenericArg::Type(t) => self.llvm_type_for_type_expr(t),
                _ => return None,
            }
        } else {
            match &rep_val.kind {
                ExprKind::Integer(_, _) => self.context.i64_type().into(),
                ExprKind::Bool(_) => self.context.bool_type().into(),
                ExprKind::Float(_, _) => self.context.f64_type().into(),
                _ => return None,
            }
        };
        let arr_ty = elem_llvm_ty.array_type(n);
        let fn_val = self.current_fn?;
        let alloca = self.create_entry_alloca(fn_val, name, arr_ty.into());
        let total_size = arr_ty.size_of()?;
        let memset_result = self.builder.build_memset(
            alloca,
            1, // align 1 — LLVM picks up the alloca's natural alignment
            self.context.i8_type().const_zero(),
            total_size,
        );
        if let Err(e) = memset_result {
            return Some(Err(format!("build_memset failed: {:?}", e)));
        }
        self.variables.insert(
            name.to_string(),
            VarSlot {
                ptr: alloca,
                ty: arr_ty.into(),
            },
        );
        Some(Ok(()))
    }

    /// Empty array literal fast path: `let a: Array[T, 0] = []`.
    ///
    /// `compile_array_literal([])` cannot infer the element type from the
    /// (empty) element list, so it returns a scalar `i64 0` sentinel — which
    /// then registers the binding's slot type as `i64`, not `[0 x T]`. At a
    /// call site that wants `Slice[T]`, `coerce_to_slice`'s Array → Slice
    /// branch keys on `slot.ty` being an `ArrayType`, so it skips the empty
    /// array and the raw `i64` reaches the call — failing LLVM verification
    /// with a `{ptr, i64}`-vs-`i64` param mismatch (B-2026-06-14-30). The
    /// `Array[T, 0]` annotation carries the element type the literal lacks, so
    /// allocate a real `[0 x T]` slot here; the binding then coerces to a
    /// zero-length slice header like any other array (the interpreter already
    /// accepts the same `[] -> Slice` shape). Mirrors the zero-init repeat-let
    /// path above, for the `[]` (empty `ArrayLiteral`) case it does not cover.
    pub(super) fn try_emit_empty_array_let(
        &mut self,
        name: &str,
        value: &Expr,
        ty: Option<&TypeExpr>,
    ) -> Option<Result<(), String>> {
        let ExprKind::ArrayLiteral(elems) = &value.kind else {
            return None;
        };
        if !elems.is_empty() {
            return None;
        }
        // Recover the element type from the `Array[T, N]` annotation; without
        // it there is no way to type the empty literal, so leave it to the
        // scalar-sentinel fallback (no worse than today).
        let te = ty?;
        let TypeKind::Path(path) = &te.kind else {
            return None;
        };
        if path.segments.first().map(|s| s.as_str()) != Some("Array") {
            return None;
        }
        let args = path.generic_args.as_ref()?;
        if args.len() != 2 {
            return None;
        }
        let elem_llvm_ty: BasicTypeEnum<'ctx> = match &args[0] {
            GenericArg::Type(t) => self.llvm_type_for_type_expr(t),
            _ => return None,
        };
        // An empty literal has length 0 by construction.
        let arr_ty = elem_llvm_ty.array_type(0);
        let fn_val = self.current_fn?;
        let alloca = self.create_entry_alloca(fn_val, name, arr_ty.into());
        self.variables.insert(
            name.to_string(),
            VarSlot {
                ptr: alloca,
                ty: arr_ty.into(),
            },
        );
        Some(Ok(()))
    }

    /// Compile `[value; count]` / `Array[value; count]`. Produces an LLVM
    /// array value `[N x T]` whose every element is the compiled `value`.
    /// `count` must be a non-negative integer literal (mirrors the
    /// typechecker's `Array[T, N]` size constraint).
    ///
    /// `Vec[v; n]` prefix form needs heap allocation + fill and is not
    /// implemented here yet — it errors with a clear message rather than
    /// silently producing the wrong shape.
    pub(super) fn compile_repeat_literal(
        &mut self,
        type_name: Option<&str>,
        value: &Expr,
        count: &Expr,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        if matches!(type_name, Some("Vec")) {
            return Err("codegen: Vec[v; n] repeat literal not yet supported".to_string());
        }
        let n = match &count.kind {
            ExprKind::Integer(n, _) if *n >= 0 => *n as u32,
            _ => {
                return Err(
                    "repeat-literal count must be a non-negative integer literal".to_string(),
                );
            }
        };
        let val = self.compile_expr(value)?;
        let elem_ty = val.get_type();
        let arr_ty = elem_ty.array_type(n);

        // Zero-value fast path. When `val` is the zero/null/false value of
        // its type, emit a single LLVM `zeroinitializer` constant — a
        // single IR token regardless of N. Covers `[0; N]`, `[false; N]`,
        // `[0.0; N]`, `[null; N]` — the common stack-array initialization
        // shapes (lookup tables, sieve buffers, zero-filled work arrays).
        // O(1) compile time in N; works at any size LLVM can represent.
        let is_zero = match val {
            BasicValueEnum::IntValue(iv) => iv.get_zero_extended_constant() == Some(0),
            BasicValueEnum::FloatValue(fv) => {
                fv.get_constant().is_some_and(|(v, _)| v.to_bits() == 0)
            }
            BasicValueEnum::PointerValue(pv) => pv.is_null(),
            _ => false,
        };
        if is_zero {
            return Ok(arr_ty.const_zero().into());
        }

        // Non-zero compile-time constant: emit one LLVM `const_array`,
        // capped at SAFE_CONST_ARRAY_N. Above that cap LLVM's downstream
        // passes crash on the giant constant (verified SIGSEGV at
        // N=80_000+ on i64 / bool); the cap is conservative.
        const SAFE_CONST_ARRAY_N: u32 = 4096;
        if n <= SAFE_CONST_ARRAY_N {
            if let Some(agg) = match val {
                BasicValueEnum::IntValue(iv) if iv.is_const() => {
                    Some(iv.get_type().const_array(&vec![iv; n as usize]))
                }
                BasicValueEnum::FloatValue(fv) if fv.is_const() => {
                    Some(fv.get_type().const_array(&vec![fv; n as usize]))
                }
                BasicValueEnum::PointerValue(pv) if pv.is_const() => {
                    Some(pv.get_type().const_array(&vec![pv; n as usize]))
                }
                _ => None,
            } {
                return Ok(agg.into());
            }
        }

        // Above the cap or for runtime values: per-element `insertvalue`.
        // Also size-capped (each element adds an IR instruction). Beyond
        // the cap we error with a pointer to the workaround rather than
        // silently producing pathologically slow IR (or, worse, crashing
        // LLVM as the previous unbounded const_array path did).
        const SAFE_INSERT_N: u32 = 1024;
        if n > SAFE_INSERT_N {
            return Err(format!(
                "codegen: repeat literal `[v; {n}]` exceeds the safe size cap ({SAFE_INSERT_N}) \
                 for non-zero / runtime values. For large arrays, use a zero initializer \
                 (`[0; {n}]`, `[false; {n}]`, etc.) which compiles in O(1) regardless of size, \
                 then fill via a runtime for-loop: `let mut buf: Array[T, {n}] = [0; {n}]; \
                 for i in 0..{n} {{ buf[i] = v; }}`."
            ));
        }
        let mut agg = arr_ty.get_undef();
        for idx in 0..n {
            agg = self
                .builder
                .build_insert_value(agg, val, idx, "rep.elem")
                .unwrap()
                .into_array_value();
        }
        Ok(agg.into())
    }

    /// `ref`/`mut ref Array[T, N]` index target (B-2026-06-17-1).
    ///
    /// A borrowed fixed array param's slot holds the BORROW — an alloca
    /// containing a `ptr` to the caller's `[N x T]` storage — and its slot LLVM
    /// type is therefore `ptr`, not `[N x T]`. The generic `compile_index` /
    /// `compile_index_store` tail dispatches on `slot.ty` and so falls past its
    /// `ArrayType` branch into the "non-array type" error. `ref_params[name]`
    /// already records the inner `[N x T]` array type (`inner_type_of_ref`), so
    /// here we load the data pointer from the slot and hand back that pointer +
    /// the `[N x T]` type; the existing `ArrayType` arm then bounds-checks and
    /// GEPs exactly as for a local array. Returns `None` for any binding that is
    /// not a ref-Array param (owned arrays, ref Vec/Slice/Map/etc. — all routed
    /// elsewhere), so non-Array dispatch is untouched. This mirrors the explicit
    /// `ref Vec[T]` route, which exists for the same `ptr`-slot reason.
    fn ref_array_index_target(
        &self,
        name: &str,
    ) -> Option<(PointerValue<'ctx>, BasicTypeEnum<'ctx>)> {
        let slot = self.variables.get(name)?;
        let arr_ty = self.ref_params.get(name).copied()?;
        if !matches!(arr_ty, BasicTypeEnum::ArrayType(_)) {
            return None;
        }
        let data_ptr = self
            .builder
            .build_load(
                self.context.ptr_type(AddressSpace::default()),
                slot.ptr,
                "refarr.data.ptr",
            )
            .unwrap()
            .into_pointer_value();
        Some((data_ptr, arr_ty))
    }

    pub(super) fn compile_index(
        &mut self,
        object: &Expr,
        index: &Expr,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        // Range indexing (`v[a..b]`): produces a Slice[T] value regardless
        // of whether `v` is an Array, Vec, or Slice. The source element
        // type is inferred from the object variable.
        if let ExprKind::Range {
            start,
            end,
            inclusive,
        } = &index.kind
        {
            // String slicing `s[a..b]` -> a fresh `String` (phase-8 line
            // 737), NOT a `Slice[T]`. `infer_elem_from_source` returns None
            // for a String, so this must be handled before the array/slice
            // element path below (and before the integer-index tail, which
            // would otherwise silently miscompile). `string_typed_exprs` is
            // the typechecker's per-expression String flag, keyed by span.
            if self
                .string_typed_exprs
                .contains(&(object.span.offset, object.span.length))
            {
                return self.compile_string_slice(object, start, end, *inclusive);
            }
            if let Some(elem_ty) = self.infer_elem_from_source(object) {
                return self.compile_range_slice(object, start, end, *inclusive, elem_ty);
            }
            // Standalone range-slice of an anonymous array/Vec literal —
            // `let x = [1, 2, 3][a..b]`. The call-arg form (`f([1,2,3][a..b])`)
            // works via `coerce_to_slice`, but a standalone binding routes
            // here, where the literal's element type isn't recoverable
            // (`infer_elem_from_source` needs a named source, and a Vec
            // literal's element type is LLVM-erased — driving the slice with
            // the wrong stride would silently miscompile). Fail loud with an
            // actionable message instead of falling through to a confusing
            // downstream "no handler"/"non-array type" error at the use site.
            // The interpreter accepts this form; binding the literal first
            // (`let a = [...]; let x = a[lo..hi];`) works in compiled mode too.
            if matches!(&object.kind, ExprKind::ArrayLiteral(_))
                || matches!(&object.kind, ExprKind::PrefixCollectionLiteral { type_name, .. }
                    if type_name == "Vec")
            {
                return Err(format!(
                    "range-slicing an anonymous array/Vec literal is not supported \
                     in compiled mode at {}:{}; bind it to a variable first \
                     (`let a = [...]; let x = a[lo..hi];`)",
                    object.span.line, object.span.column
                ));
            }
        }

        // Nested indexed read (`grid[i][j]` / `factors[v][0]`): the
        // outer container's element type is itself a Vec / Slice /
        // Array, so the inner Index expression yields an aggregate
        // value that the generic fall-through can't handle. Lower
        // the inner index to an element pointer via the existing
        // indexed-receiver machinery, mint a synth identifier so the
        // recursive dispatch sees a regular variable, and recurse.
        // Single-level nesting only — chained `a[i][j][k]` rejected
        // upstream by `compile_indexed_receiver_method`'s MR5 guard,
        // applied symmetrically here.
        if let ExprKind::Index {
            object: inner,
            index: inner_idx,
        } = &object.kind
        {
            return self.compile_nested_index_read(inner, inner_idx, index);
        }

        // Field-access-rooted indexing (`h.items[i]`, `node.neighbors[i]`,
        // incl. shared-struct receivers and `outer[i].field[j]`): resolve
        // the field's storage pointer + TypeExpr via the FR-slice helper,
        // mint a synth identifier with the field's registries populated,
        // and recurse so the identifier-keyed dispatch above (Vec / Slice /
        // Map / generic Array) handles the actual index. Without this arm
        // the generic tail compiles the field access to a struct VALUE
        // (e.g. Vec's `{ptr,len,cap}`) in a temp alloca and dies on
        // "Index operator applied to non-array type" — the kata-133-audit
        // bug (2026-06-05). `Ok(None)` (unknown struct/field shape) falls
        // through to the existing paths unchanged.
        if let ExprKind::FieldAccess {
            object: inner,
            field,
        } = &object.kind
        {
            // `self.field[i]` — `self` parses as `SelfValue`, but
            // `lower_field_access_ptr`'s receiver match only recognises
            // `Identifier` / `Index` inners (SelfValue is deliberately left
            // to return `Ok(None)` there so `self.field.method()` dispatch —
            // e.g. atomic `self.count.fetch_add(...)` — keeps falling through
            // to its dedicated handler, NOT the generic field-receiver path).
            // `self` is registered under the name "self" in every per-binding
            // registry, so for the INDEX path specifically we normalise the
            // receiver to a synthetic `Identifier("self")` and resolve it via
            // the Identifier arm. Scoped here (not in the shared helper) so
            // the method-receiver path is byte-identical to before — without
            // this, `self.bytes[self.current]` dies on "Index operator applied
            // to non-array type" (self-hosting lexer).
            let self_ident;
            let inner: &Expr = if matches!(inner.kind, ExprKind::SelfValue) {
                self_ident = Expr {
                    kind: ExprKind::Identifier("self".to_string()),
                    span: inner.span.clone(),
                };
                &self_ident
            } else {
                inner
            };
            if let Some((field_ptr, field_ll_ty, field_te)) =
                self.lower_field_access_ptr(inner, field, "index expression")?
            {
                let synth = format!("__field_elem_{}", self.indexed_elem_counter);
                self.indexed_elem_counter += 1;
                self.variables.insert(
                    synth.clone(),
                    super::state::VarSlot {
                        ptr: field_ptr,
                        ty: field_ll_ty,
                    },
                );
                self.register_var_from_type_expr(&synth, &field_te);
                let synth_expr = Expr {
                    kind: ExprKind::Identifier(synth.clone()),
                    span: object.span.clone(),
                };
                let result = self.compile_index(&synth_expr, index);

                // Clean up synth registrations (same set as the FR slice).
                self.variables.remove(&synth);
                self.vec_elem_types.remove(&synth);
                self.slice_elem_types.remove(&synth);
                self.var_elem_type_exprs.remove(&synth);
                self.var_type_names.remove(&synth);
                self.map_key_types.remove(&synth);
                self.map_val_types.remove(&synth);
                self.map_key_type_names.remove(&synth);
                self.map_key_type_exprs.remove(&synth);
                self.set_elem_types.remove(&synth);
                self.set_elem_type_names.remove(&synth);
                self.set_elem_type_exprs.remove(&synth);

                return result;
            }
        }

        // Tensor variable indexing: `t[i, j, k]` (the parser desugars to
        // a single tuple index). Routed before the Vec/Slice paths —
        // tensor bindings are single-pointer slots and dispatch through
        // the `[rank][dims][data]` layout helpers in
        // `src/codegen/tensor.rs`.
        if let ExprKind::Identifier(name) = &object.kind {
            if let Some(info) = self.tensor_var_infos.get(name.as_str()).cloned() {
                let t_ptr = self.tensor_ptr_for_var(name)?;
                return self.compile_tensor_index(t_ptr, &info, index);
            }
        }

        // Slice variable indexing: before the fast-path alloca lookup, check
        // whether the object is a slice variable. Slices use a 2-field
        // `{ptr, len}` representation and dispatch to a dedicated path.
        if let ExprKind::Identifier(name) = &object.kind {
            if self.slice_elem_types.contains_key(name.as_str()) {
                return self.compile_slice_index(name, index);
            }
        }

        // Map variable indexing: `m[k]` calls karac_map_get and panics on miss.
        // The key is hashed via the per-K hash_fn registered at Map construction;
        // it does NOT need to be an integer (unlike Array/Vec/Slice).
        if let ExprKind::Identifier(name) = &object.kind {
            if self.map_key_types.contains_key(name.as_str()) {
                return self.compile_map_index(name, index);
            }
        }

        // SoA-laid-out Vec indexing: `entities[i]` materializes the AoS
        // element struct from the per-group buffers. Detected by the var
        // name matching a registered layout (SoA-ness is codegen-only —
        // the typechecker sees a plain `Vec[Entity]`), mirroring the
        // method-dispatch check in `compile_indexed_receiver_method`'s
        // sibling at the SoA `.push()` / `.len()` site. Routed before the
        // Vec branch because SoA vars are never registered in
        // `vec_elem_types`; without this they'd fall through to the
        // "non-array type" error at the tail.
        if let ExprKind::Identifier(name) = &object.kind {
            if self.soa_layouts.contains_key(name.as_str()) {
                return self.compile_soa_index_read(name, index);
            }
        }

        // Vec variable indexing: route through `compile_vec_index` so both
        // owned and `ref Vec[T]` forms work. The downstream slot.ty branch
        // can't handle ref Vecs — for them slot.ty is `ptr`, not the Vec
        // struct type, so the StructType arm below would never fire.
        //
        // Bypass the Vec routing when the slot's LLVM type is `ArrayType` —
        // i.e. the `let a = [1, 2, 3]` shape where the typechecker recorded
        // "Vec" for the binding (synthesis-mode default) but
        // `compile_array_literal` produced an `[N x T]` aggregate that
        // bind_pattern alloca'd as ArrayType. Vec dispatch on an Array
        // alloca lays the `{ptr, i64, i64}` view over `[N x T]` bytes and
        // GEPs produce wild pointers (first i64 loaded as data ptr,
        // second i64 as len → out-of-bounds garbage writes / hangs at
        // runtime). Fall through to the Array path below in that case.
        if let ExprKind::Identifier(name) = &object.kind {
            if self.vec_elem_types.contains_key(name.as_str()) {
                let slot_is_array = self
                    .variables
                    .get(name.as_str())
                    .is_some_and(|s| matches!(s.ty, BasicTypeEnum::ArrayType(_)));
                if !slot_is_array {
                    return self.compile_vec_index(name, index);
                }
            }
        }

        // Inline index of a method/function-returned `Vec` (`a.shape()[k]`,
        // `f()[i]`): the object is a non-place expression whose value is a
        // `Vec` `{ptr, len, cap}` struct. The identifier-keyed Vec path above
        // fires only for named bindings; an arbitrary Vec-producing expr
        // otherwise reaches the generic tail below, which stores the struct
        // to a temp and dispatches only `ArrayType` / `VectorType` — a Vec
        // struct falls through to the "non-array type" error. Mirror the
        // FieldAccess synth-var arm: materialize the value into a synth Vec
        // local, recurse so the identifier Vec path lowers the index, then
        // drop the temp Vec (buffer + nested element heap) after the read.
        //
        // The Vec element `TypeExpr` is resolved from the callee's return
        // signature (`inline_temp_vec_te`), NOT from `owned_temp_drops`:
        // the parser gives every postfix expr the receiver's span, so a
        // `Call`/`Index` pair on `make()[i]` collide, and the `Index`'s
        // element-type clobbers the `Call`'s `Vec` in `expr_types` (and
        // hence in `owned_temp_drops`). The signature lookup sidesteps the
        // collision entirely. (Phase-11 longtail: shape-generic `matmul`
        // reading dims via `a.shape()[k]`, 2026-06-09.)
        if let Some(vec_te) = self.inline_temp_vec_te(object) {
            return self.compile_inline_temp_vec_index(object, index, &vec_te);
        }

        let idx_val = self.compile_expr(index)?.into_int_value();
        let i64_t = self.context.i64_type();

        // Get a pointer to the array storage.
        // Fast path: if the object is a local variable, use its alloca
        // directly. Module-level `let X: Array[T, N] = […]` bindings
        // (slice 10) have an LLVM global rather than an alloca, but the
        // pointer + type carries the same role — we use the global's
        // pointer and the binding's recorded llvm_ty for the GEP.
        let (arr_ptr, arr_ty) = if let ExprKind::Identifier(name) = &object.kind {
            if let Some((data_ptr, ref_arr_ty)) = self.ref_array_index_target(name) {
                (data_ptr, ref_arr_ty)
            } else if let Some(slot) = self.variables.get(name.as_str()).copied() {
                (slot.ptr, slot.ty)
            } else if let Some(info) = self.module_bindings.get(name.as_str()) {
                (info.global.as_pointer_value(), info.llvm_ty)
            } else {
                return Err(format!("Undefined variable '{}' in index expression", name));
            }
        } else {
            // Arbitrary expression: compile, store to temp alloca, use that pointer.
            let arr_val = self.compile_expr(object)?;
            let fn_val = self.current_fn.unwrap();
            let tmp = self.create_entry_alloca(fn_val, "arr.tmp", arr_val.get_type());
            self.builder.build_store(tmp, arr_val).unwrap();
            (tmp, arr_val.get_type())
        };

        // Bounds check: panic if index >= array_length.
        if let BasicTypeEnum::ArrayType(at) = arr_ty {
            let len = i64_t.const_int(at.len() as u64, false);
            let fn_val = self.current_fn.unwrap();
            let oob_bb = self.context.append_basic_block(fn_val, "idx.oob");
            let ok_bb = self.context.append_basic_block(fn_val, "idx.ok");
            let cmp = self
                .builder
                .build_int_compare(inkwell::IntPredicate::UGE, idx_val, len, "bounds")
                .unwrap();
            self.builder
                .build_conditional_branch(cmp, oob_bb, ok_bb)
                .unwrap();

            // OOB path: call abort or unreachable.
            self.builder.position_at_end(oob_bb);
            self.emit_panic("array index out of bounds");
            self.builder.build_unreachable().unwrap();

            // OK path: GEP + load.
            self.builder.position_at_end(ok_bb);
            let zero = i64_t.const_int(0, false);
            let elem_ptr = unsafe {
                self.builder
                    .build_gep(arr_ty, arr_ptr, &[zero, idx_val], "arr.elem.ptr")
                    .unwrap()
            };
            let elem_ty = at.get_element_type();
            let val = self
                .builder
                .build_load(elem_ty, elem_ptr, "arr.elem")
                .unwrap();
            Ok(val)
        } else if let BasicTypeEnum::VectorType(vt) = arr_ty {
            // SIMD lane read `v[i] -> T` (design.md § Portable SIMD). The slot
            // holds the `<N x T>` value directly (not pointer-backed), so we
            // bounds-check the lane index, load the vector, and extractelement
            // rather than GEP into memory.
            let len = i64_t.const_int(vt.get_size() as u64, false);
            let fn_val = self.current_fn.unwrap();
            let oob_bb = self.context.append_basic_block(fn_val, "lane.oob");
            let ok_bb = self.context.append_basic_block(fn_val, "lane.ok");
            let cmp = self
                .builder
                .build_int_compare(inkwell::IntPredicate::UGE, idx_val, len, "lane.bounds")
                .unwrap();
            self.builder
                .build_conditional_branch(cmp, oob_bb, ok_bb)
                .unwrap();

            self.builder.position_at_end(oob_bb);
            self.emit_panic("vector lane index out of bounds");
            self.builder.build_unreachable().unwrap();

            self.builder.position_at_end(ok_bb);
            let vec_val = self
                .builder
                .build_load(arr_ty, arr_ptr, "vec.val")
                .unwrap()
                .into_vector_value();
            let lane = self
                .builder
                .build_extract_element(vec_val, idx_val, "lane")
                .map_err(|e| format!("vector extractelement failed: {e}"))?;
            Ok(lane)
        } else {
            // Vec, Slice, Map already routed through their dedicated paths
            // above. Anything still reaching here is genuinely not indexable.
            Err("Index operator applied to non-array type".to_string())
        }
    }

    /// Resolve the `Vec[T]` / `VecDeque[T]` `TypeExpr` an inline-indexed
    /// temporary (`make()[i]`, `a.shape()[k]`) produces — the element type
    /// needed to register the synth Vec local and to drop/clone correctly.
    ///
    /// Resolved from the callee *signature*, not `owned_temp_drops`: the
    /// parser stamps every postfix expr with the receiver's span, so the
    /// `Call` and its wrapping `Index` share a span and the `Index` result
    /// type clobbers the `Call`'s `Vec` in `expr_types` (the table
    /// `owned_temp_drops` is built from). `None` for anything that isn't a
    /// fresh-owned Vec temporary — a non-Vec return, a `ref`-returning
    /// callee (a borrow, returned as a `ptr`, must not be freed), or an
    /// unrecognized receiver — leaving the generic path to handle it.
    fn inline_temp_vec_te(&self, object: &Expr) -> Option<TypeExpr> {
        match &object.kind {
            // Free-function call: the declared return `TypeExpr`. A direct
            // (non-`ref`) `Vec[T]` / `VecDeque[T]` return is a fresh owned
            // temp; a `Ref`/`MutRef` return is a borrow and is excluded.
            ExprKind::Call { callee, .. } => {
                let ExprKind::Identifier(name) = &callee.kind else {
                    return None;
                };
                let te = self.fn_return_type_exprs.get(name)?;
                if matches!(te.kind, TypeKind::Ref(_) | TypeKind::MutRef(_)) {
                    return None;
                }
                self.extract_vec_elem_type(te).is_some().then(|| te.clone())
            }
            // `tensor.shape()` is the one built-in producing a fresh owned
            // `Vec[i64]` (`compile_tensor_shape_method`). Gated on a tensor
            // receiver — an identifier tensor var or a tensor-typed chained
            // receiver (`h.view().shape()`) — so a hypothetical user method
            // named `shape` returning some other type can't match.
            ExprKind::MethodCall {
                object: recv,
                method,
                ..
            } if method == "shape" => {
                let recv_is_tensor = match &recv.kind {
                    ExprKind::Identifier(n) => self.tensor_var_infos.contains_key(n.as_str()),
                    _ => self
                        .tensor_typed_exprs
                        .contains_key(&(recv.span.offset, recv.span.length)),
                };
                if !recv_is_tensor {
                    return None;
                }
                let sp = recv.span.clone();
                let i64_te = TypeExpr {
                    kind: TypeKind::Path(PathExpr {
                        segments: vec!["i64".to_string()],
                        generic_args: None,
                        span: sp.clone(),
                    }),
                    span: sp.clone(),
                };
                Some(TypeExpr {
                    kind: TypeKind::Path(PathExpr {
                        segments: vec!["Vec".to_string()],
                        generic_args: Some(vec![GenericArg::Type(i64_te)]),
                        span: sp.clone(),
                    }),
                    span: sp,
                })
            }
            _ => None,
        }
    }

    /// Index a freshly-produced, owned `Vec` temporary (`a.shape()[k]`,
    /// `make_vec()[i]`) — the value `object` evaluates to is a `Vec`
    /// `{ptr, len, cap}` struct, not a named binding the identifier-keyed
    /// Vec path can dispatch on. Materializes the value into a synth Vec
    /// local (so the existing `compile_vec_index` lowering runs unchanged),
    /// reads the element, deep-clones it when the element type isn't
    /// trivially Copy (the read shallow-aliases the about-to-be-freed
    /// buffer otherwise), then drops the temp Vec (buffer + every nested
    /// element's heap) via the Vec drop fn. `vec_te` is the temporary's
    /// `Vec[T]` / `VecDeque[T]` `TypeExpr` (from `inline_temp_vec_te`).
    fn compile_inline_temp_vec_index(
        &mut self,
        object: &Expr,
        index: &Expr,
        vec_te: &TypeExpr,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let vec_val = self.compile_expr(object)?;
        // Defensive: a `ref Vec` borrow (or any non-Vec shape that slipped
        // past the hint) yields a `ptr`, not the `{ptr, len, cap}` struct —
        // don't synth/free it; report the generic error rather than freeing
        // memory we don't own.
        if !self.llvm_ty_is_vec_struct(vec_val.get_type()) {
            return Err("Index operator applied to non-array type".to_string());
        }
        let fn_val = self.current_fn.unwrap();

        // Materialize into a synth Vec local + register so the recursion's
        // identifier-keyed Vec path (`compile_vec_index`) lowers `[index]`.
        let synth = format!("__inline_vec_{}", self.indexed_elem_counter);
        self.indexed_elem_counter += 1;
        let slot = self.create_entry_alloca(fn_val, "inline.vec.tmp", vec_val.get_type());
        self.builder.build_store(slot, vec_val).unwrap();
        self.variables.insert(
            synth.clone(),
            super::state::VarSlot {
                ptr: slot,
                ty: vec_val.get_type(),
            },
        );
        self.register_var_from_type_expr(&synth, vec_te);

        let synth_expr = Expr {
            kind: ExprKind::Identifier(synth.clone()),
            span: object.span.clone(),
        };
        let elem = self.compile_index(&synth_expr, index)?;

        // The element value shallow-aliases buffer memory for non-Copy
        // element types (a `String` / `Vec` element's inner `data` ptr is
        // shared); deep-clone it before the buffer's nested heap is freed so
        // the returned value stands alone. Trivially-Copy elements (the
        // `Vec[i64]` `shape()` case) are already standalone scalars.
        let inner_te = vec_inner_type_expr(vec_te);
        let result = match &inner_te {
            Some(elem_te) if !super::vec_method::is_trivially_copyable_te(elem_te) => {
                let elem_ll = elem.get_type();
                let src = self.create_entry_alloca(fn_val, "inline.elem.src", elem_ll);
                self.builder.build_store(src, elem).unwrap();
                let dst = self.create_entry_alloca(fn_val, "inline.elem.clone", elem_ll);
                let clone_fn = self.emit_clone_fn_for_type_expr(elem_te);
                self.builder
                    .build_call(clone_fn, &[src.into(), dst.into()], "")
                    .unwrap();
                self.builder
                    .build_load(elem_ll, dst, "inline.elem.cloned")
                    .unwrap()
            }
            _ => elem,
        };

        // Free the temp Vec: buffer + (for non-Copy elements) each element's
        // nested heap. Safe because the returned element was deep-cloned.
        let drop_fn = self.emit_drop_fn_for_type_expr(vec_te);
        self.builder
            .build_call(drop_fn, &[slot.into()], "")
            .unwrap();

        // Tidy the synth registrations (same set `register_var_from_type_expr`
        // could have touched for a Vec / VecDeque / String binding).
        self.variables.remove(&synth);
        self.vec_elem_types.remove(&synth);
        self.var_elem_type_exprs.remove(&synth);
        self.var_type_names.remove(&synth);
        self.string_vars.remove(&synth);

        Ok(result)
    }

    /// `let w = v[i]` where `v: Vec[T]` and `T` is a heap type (String / Vec /
    /// heap struct / enum): `compile_index` returns a **shallow** copy of the
    /// element `{ptr, len, cap}` struct, whose inner `data` pointer aliases `v`'s
    /// element buffer. Binding it owned arms the new binding's scope-exit drop on
    /// that shared buffer, and `v`'s own element-drop frees it too — a double-free
    /// at scope exit (B-2026-06-14-11, the Vec-index sibling of the move-out-of-
    /// container family). Indexing does NOT consume the element (the interpreter,
    /// the oracle, leaves `v[i]` valid and clones), so the fix is a deep clone:
    /// the binding gets its own buffer, `v[i]` is untouched. Returns `val`
    /// unchanged for a non-Vec index, a String range slice (already a fresh owned
    /// buffer), or a trivially-Copy element. Same clone path
    /// `compile_inline_temp_vec_index` uses for temp-Vec elements.
    /// True when `expr` is a plain element index (`v[i]`, not a `v[a..b]` range)
    /// into a named `Vec` whose element type is non-trivially-copyable (a heap
    /// String/Vec/Map/enum/struct element). Mirrors the gates in
    /// `clone_owned_vec_index_element` — used by the match path to decide whether
    /// a `match v[i] { … }` scrutinee must be deep-cloned before destructuring.
    pub(super) fn expr_is_heap_vec_index(&self, expr: &Expr) -> bool {
        let ExprKind::Index { object, index } = &expr.kind else {
            return false;
        };
        if matches!(&index.kind, ExprKind::Range { .. }) {
            return false;
        }
        let ExprKind::Identifier(name) = &object.kind else {
            return false;
        };
        match self.var_elem_type_exprs.get(name.as_str()) {
            Some(elem_te) => !super::vec_method::is_trivially_copyable_te(elem_te),
            None => false,
        }
    }

    /// True when `expr` is a plain element index (`make()[i]`, not a range)
    /// into a *fresh-owned `Vec` temporary* (`inline_temp_vec_te` matches the
    /// callee's return signature) whose element type is non-trivially-copyable.
    /// `compile_inline_temp_vec_index` lowers that shape by **deep-cloning** the
    /// indexed element into a fresh owned buffer (the temp Vec is dropped right
    /// after the read, so the read must stand alone), then de-registers the
    /// synth Vec local — so the clone has NO consuming binding and NO scope-exit
    /// cleanup of its own. In a directly-consuming position (a `println`/`print`
    /// argument, a by-value user-fn argument) the consumer must free it exactly
    /// like a `Call`/`MethodCall` fresh temp (`expr_yields_fresh_owned_temp`),
    /// or it leaks once per call — the reported `println(names()[0])`
    /// (B-2026-06-14-32). A trivially-Copy element (`shape()[k] -> i64`) returns
    /// a bare scalar — no clone, nothing to free — so it is excluded, matching
    /// the clone gate in `compile_inline_temp_vec_index`.
    pub(super) fn expr_is_inline_temp_vec_heap_index(&self, expr: &Expr) -> bool {
        let ExprKind::Index { object, index } = &expr.kind else {
            return false;
        };
        if matches!(&index.kind, ExprKind::Range { .. }) {
            return false;
        }
        let Some(vec_te) = self.inline_temp_vec_te(object) else {
            return false;
        };
        match vec_inner_type_expr(&vec_te) {
            Some(elem_te) => !super::vec_method::is_trivially_copyable_te(&elem_te),
            None => false,
        }
    }

    pub(super) fn clone_owned_vec_index_element(
        &mut self,
        value: &Expr,
        val: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let ExprKind::Index { object, index } = &value.kind else {
            return Ok(val);
        };
        // A String *range* slice (`s[a..b]`) already returns a freshly-allocated
        // owned buffer — only a plain element index (`v[i]`) shallow-aliases.
        if matches!(&index.kind, ExprKind::Range { .. }) {
            return Ok(val);
        }
        // Element TypeExpr of the indexed object. Named-Vec var only: `v[i]`,
        // not `self.field[i]` / `matrix[i][j]` (those keep the prior behaviour;
        // the named-binding case is the common — and reported — one).
        let ExprKind::Identifier(name) = &object.kind else {
            return Ok(val);
        };
        let Some(elem_te) = self.var_elem_type_exprs.get(name.as_str()).cloned() else {
            return Ok(val);
        };
        if super::vec_method::is_trivially_copyable_te(&elem_te) {
            return Ok(val);
        }
        // Deep-clone via the per-type clone fn (src/dst slots), exactly as
        // `compile_inline_temp_vec_index`.
        let fn_val = self.current_fn.unwrap();
        let elem_ll = val.get_type();
        let src = self.create_entry_alloca(fn_val, "vidx.elem.src", elem_ll);
        self.builder.build_store(src, val).unwrap();
        let dst = self.create_entry_alloca(fn_val, "vidx.elem.clone", elem_ll);
        let clone_fn = self.emit_clone_fn_for_type_expr(&elem_te);
        self.builder
            .build_call(clone_fn, &[src.into(), dst.into()], "")
            .unwrap();
        Ok(self
            .builder
            .build_load(elem_ll, dst, "vidx.elem.cloned")
            .unwrap())
    }

    /// #38 — deep-clone a `match <self.field[i]>.enumfield { … }` scrutinee
    /// value. The indexed object is itself a place (FieldAccess/`self`), e.g. the
    /// parser's `self.tokens[self.pos].token`. The #15/#18 source-suppression
    /// can't reach a FieldAccess-rooted Vec element's field
    /// (`field_chain_place_ptr` bails on a non-Identifier Index root), so a heap
    /// payload bound OUT of this scrutinee would SHALLOW-ALIAS the Vec element's
    /// buffer; when the container (the `Parser`'s `Vec[SpannedToken]`) drops, it
    /// frees that buffer and the bound value (now in the returned AST) dangles —
    /// a use-after-free that surfaced as empty `String`s in the self-hosted
    /// parser. Cloning the enum value here makes each binding own an INDEPENDENT
    /// buffer; the source element is freed normally (single free). The sibling
    /// `clone_owned_vec_index_element` covers the plain-var `v[i]` case (a direct
    /// `Index` scrutinee); a plain-var `v[i].field` is left to the #18
    /// suppression (which DOES resolve it), so this fires only for the
    /// FieldAccess-rooted-Index field shape the suppression misses.
    /// Returns `(value, did_clone)` — `did_clone` is `true` only when an
    /// independent deep clone was actually emitted (the FieldAccess-rooted
    /// Vec-index heap-enum shape). The caller uses it to force the cloned
    /// value through `materialize_freshtemp_enum_scrutinee` so the clone-temp
    /// is drop-tracked (a no-bind arm frees it; a bound arm's per-field
    /// suppression zeroes the CLONE's moved-out caps) — without that, the
    /// clone leaks once [#35] makes the container drop free the original
    /// element (the clone's reachability anchor vanishes). The original Vec
    /// element is left intact for the container's element drain.
    pub(super) fn clone_borrowed_index_field_enum_scrutinee(
        &mut self,
        value: &Expr,
        val: BasicValueEnum<'ctx>,
    ) -> Result<(BasicValueEnum<'ctx>, bool), String> {
        let ExprKind::FieldAccess { object, field } = &value.kind else {
            return Ok((val, false));
        };
        // Only a FieldAccess-rooted Vec index (`self.toks[i].field`); a plain-var
        // `v[i].field` is handled by the #18 suppression — cloning it too would
        // leave the cap-zeroed source unfreed (a leak).
        let ExprKind::Index {
            object: idx_obj, ..
        } = &object.kind
        else {
            return Ok((val, false));
        };
        if matches!(idx_obj.kind, ExprKind::Identifier(_)) {
            return Ok((val, false));
        }
        let Some(obj_ty) = self.place_chain_type_name(object) else {
            return Ok((val, false));
        };
        let Some(fidx) = self
            .struct_field_names
            .get(obj_ty.as_str())
            .and_then(|ns| ns.iter().position(|n| n == field))
        else {
            return Ok((val, false));
        };
        let Some(field_te) = self
            .struct_field_type_exprs
            .get(obj_ty.as_str())
            .and_then(|tes| tes.get(fidx))
            .cloned()
        else {
            return Ok((val, false));
        };
        // Only a heap-bearing field needs the clone (a Copy field can't dangle).
        if super::vec_method::is_trivially_copyable_te(&field_te) {
            return Ok((val, false));
        }
        let fn_val = self.current_fn.unwrap();
        let ll = val.get_type();
        let src = self.create_entry_alloca(fn_val, "fld.enum.src", ll);
        self.builder.build_store(src, val).unwrap();
        let dst = self.create_entry_alloca(fn_val, "fld.enum.clone", ll);
        let clone_fn = self.emit_clone_fn_for_type_expr(&field_te);
        self.builder
            .build_call(clone_fn, &[src.into(), dst.into()], "")
            .unwrap();
        Ok((
            self.builder.build_load(ll, dst, "fld.enum.cloned").unwrap(),
            true,
        ))
    }

    /// String slicing `s[a..b]` -> a fresh `String` (phase-8 line 737).
    /// Extracts the `{ptr, len, cap}` aggregate, resolves the byte bounds
    /// (`a..` -> end = len; `..b` -> start = 0; `a..=b` includes byte `b`),
    /// and delegates the bounds + UTF-8 char-boundary validation, allocation,
    /// and copy to the `karac_string_slice` runtime helper (which exit(1)s
    /// with `E_STRING_SLICE_NOT_AT_CHAR_BOUNDARY` / a bounds message on a bad
    /// index — mirroring the interpreter and Rust's `&s[a..b]`). The helper
    /// returns the new buffer pointer (null for an empty slice, matching
    /// `karac_string_clone`'s empty-String convention); this builds the
    /// result `{ptr, len, cap}` with `len = cap = end - start`. The result is
    /// a normal owned String, so the enclosing let/temp machinery frees it at
    /// scope exit exactly like a concat result.
    pub(super) fn compile_string_slice(
        &mut self,
        object: &Expr,
        start: &Option<Box<Expr>>,
        end: &Option<Box<Expr>>,
        inclusive: bool,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let i64_t = self.context.i64_type();

        // The String aggregate `{ptr, i64 len, i64 cap}`.
        let agg = self.compile_expr(object)?.into_struct_value();
        let data_ptr = self
            .builder
            .build_extract_value(agg, 0, "s.ptr")
            .unwrap()
            .into_pointer_value();
        let str_len = self
            .builder
            .build_extract_value(agg, 1, "s.len")
            .unwrap()
            .into_int_value();

        // start (default 0) and raw end (default len), coerced to i64.
        let start_i = match start {
            Some(e) => {
                let v = self.compile_expr(e)?;
                self.coerce_scalar_to_type(v, i64_t.into()).into_int_value()
            }
            None => i64_t.const_int(0, false),
        };
        let raw_end = match end {
            Some(e) => {
                let v = self.compile_expr(e)?;
                self.coerce_scalar_to_type(v, i64_t.into()).into_int_value()
            }
            None => str_len,
        };
        // Inclusive `a..=b` includes byte `b`, so the exclusive end is b + 1.
        let end_i = if inclusive {
            self.builder
                .build_int_add(raw_end, i64_t.const_int(1, false), "end.incl")
                .unwrap()
        } else {
            raw_end
        };

        // karac_string_slice(data, len, start, end) -> new buffer ptr.
        let new_ptr = self
            .builder
            .build_call(
                self.karac_string_slice_fn,
                &[
                    data_ptr.into(),
                    str_len.into(),
                    start_i.into(),
                    end_i.into(),
                ],
                "str.slice",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();

        let new_len = self
            .builder
            .build_int_sub(end_i, start_i, "slice.len")
            .unwrap();

        // Build the result String aggregate {ptr, len, cap} with cap == len
        // (fresh buffer, no headroom — same contract as karac_string_clone).
        let str_ty = self.vec_struct_type();
        let mut out = str_ty.get_undef();
        out = self
            .builder
            .build_insert_value(out, new_ptr, 0, "slice.ptr")
            .unwrap()
            .into_struct_value();
        out = self
            .builder
            .build_insert_value(out, new_len, 1, "slice.len.f")
            .unwrap()
            .into_struct_value();
        out = self
            .builder
            .build_insert_value(out, new_len, 2, "slice.cap")
            .unwrap()
            .into_struct_value();
        Ok(out.into())
    }

    /// Borrowed (non-allocating) sibling of `compile_string_slice`: builds a
    /// `{ptr, len, cap = 0}` String view that points *into* the source string
    /// rather than into a freshly-allocated buffer. `cap = 0` is the existing
    /// static/borrowed marker every `cap > 0` free guard skips, so the view is
    /// never freed.
    ///
    /// Sound only where the key is **not retained**: map lookup methods
    /// (`get`/`contains_key`/`remove`/`get_or`) hash+compare and discard it,
    /// and `Map.insert` over the borrowed-str path (`karac_map_insert_borrowed_str_old`)
    /// deep-copies the bytes on a fresh insertion — so the borrowed pointer
    /// never outlives the source. Used only via `try_compile_borrowed_string_slice`.
    pub(super) fn compile_string_slice_borrowed(
        &mut self,
        object: &Expr,
        start: &Option<Box<Expr>>,
        end: &Option<Box<Expr>>,
        inclusive: bool,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let i64_t = self.context.i64_type();
        let agg = self.compile_expr(object)?.into_struct_value();
        let data_ptr = self
            .builder
            .build_extract_value(agg, 0, "bs.ptr")
            .unwrap()
            .into_pointer_value();
        let str_len = self
            .builder
            .build_extract_value(agg, 1, "bs.len")
            .unwrap()
            .into_int_value();

        let start_i = match start {
            Some(e) => {
                let v = self.compile_expr(e)?;
                self.coerce_scalar_to_type(v, i64_t.into()).into_int_value()
            }
            None => i64_t.const_int(0, false),
        };
        let raw_end = match end {
            Some(e) => {
                let v = self.compile_expr(e)?;
                self.coerce_scalar_to_type(v, i64_t.into()).into_int_value()
            }
            None => str_len,
        };
        let end_i = if inclusive {
            self.builder
                .build_int_add(raw_end, i64_t.const_int(1, false), "bs.end.incl")
                .unwrap()
        } else {
            raw_end
        };

        // karac_string_slice_borrow(data, len, start, end) -> ptr into source
        // (validates bounds + char boundaries; no allocation).
        let view_ptr = self
            .builder
            .build_call(
                self.karac_string_slice_borrow_fn,
                &[
                    data_ptr.into(),
                    str_len.into(),
                    start_i.into(),
                    end_i.into(),
                ],
                "str.slice.borrow",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        // Fixed-width slices (`s[d..d+1]`, `s[2..5]`) carry a compile-time
        // width; emitting it as an i64 constant lets the consumer's copy —
        // `push_str`'s memcpy, which reads this `len` field — lower to a sized
        // store instead of a branchy variable-length copy. That runtime
        // `src_len` was the root of the v2 sweep's String-build residual
        // (`out.push_str(s[d..d+1])` 1-byte append loops). The runtime
        // `karac_string_slice_borrow` validation above is unchanged — it still
        // panics on out-of-range / non-char-boundary bounds before this point,
        // so a folded width only ever reaches the copy when it equals the
        // real `end - start`. Any runtime or unrecognized shape falls back to
        // the exact subtraction, which is always correct, just not folded.
        let view_len = match const_slice_width(start, end, inclusive) {
            Some(w) => i64_t.const_int(w, false),
            None => self
                .builder
                .build_int_sub(end_i, start_i, "bs.view.len")
                .unwrap(),
        };

        // {ptr, len, cap = 0} — cap 0 marks a non-owned view (never freed).
        let str_ty = self.vec_struct_type();
        let mut out = str_ty.get_undef();
        out = self
            .builder
            .build_insert_value(out, view_ptr, 0, "bs.out.ptr")
            .unwrap()
            .into_struct_value();
        out = self
            .builder
            .build_insert_value(out, view_len, 1, "bs.out.len")
            .unwrap()
            .into_struct_value();
        out = self
            .builder
            .build_insert_value(out, i64_t.const_zero(), 2, "bs.out.cap")
            .unwrap()
            .into_struct_value();
        Ok(out.into())
    }

    /// If `arg` is a String range-index expression (`s[a..b]` / `s[a..=b]`),
    /// compile it as a borrowed view (no allocation) and return it; otherwise
    /// `Ok(None)`. The String-ness of the sliced object is the typechecker's
    /// per-expression `string_typed_exprs` flag (same gate `compile_index`
    /// uses to route String slicing). Drives the allocation-free path for read-only String-slice consumers — `push_str` and map-key lookups (`get`/`contains_key`/…) — that only read the bytes.
    pub(super) fn try_compile_borrowed_string_slice(
        &mut self,
        arg: &Expr,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        if let ExprKind::Index { object, index } = &arg.kind {
            if let ExprKind::Range {
                start,
                end,
                inclusive,
            } = &index.kind
            {
                if self
                    .string_typed_exprs
                    .contains(&(object.span.offset, object.span.length))
                {
                    return Ok(Some(
                        self.compile_string_slice_borrowed(object, start, end, *inclusive)?,
                    ));
                }
            }
        }
        Ok(None)
    }

    /// Index into a `Vec[T]` variable: `v[i]`. Handles both owned Vec values
    /// (slot is the `{ptr,len,cap}` struct) and `ref Vec[T]` parameters
    /// (slot is a `ptr` to the caller's struct) by routing the struct-base
    /// pointer through `get_data_ptr`. Loads `len`, bounds-checks, then
    /// GEPs `data[i]` and loads the element.
    pub(super) fn compile_vec_index(
        &mut self,
        name: &str,
        index: &Expr,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let elem_ty = self.vec_elem_type_for_var(name);
        let elem_ptr = self.vec_index_elem_ptr(name, index)?;
        let val = self
            .builder
            .build_load(elem_ty, elem_ptr, "v.elem")
            .unwrap();
        Ok(val)
    }

    /// Index into a SoA-laid-out Vec variable: `entities[i]`. Materializes
    /// the AoS element struct on the fly by loading one sub-struct from
    /// each group buffer at `[i]` and scattering its fields back into the
    /// element struct at their original positions — the exact inverse of
    /// the push decomposition in `compile_soa_method`. Bounds-checked
    /// against the SoA `len`. Returning the whole element value is what
    /// lets `entities[i].field` reads work through the generic field-
    /// extract path with no SoA-specific field-access code.
    ///
    /// Primitive (non-heap) element fields only: a heap field (`String` /
    /// `Vec` stored in a group) would have its header copied here exactly
    /// as `push` copies it on the way in, aliasing the group buffer — and
    /// SoA per-element drop (the separate "SoA drop semantics" entry) is
    /// not yet implemented, so heap-bearing SoA elements are out of scope
    /// until that lands.
    pub(super) fn compile_soa_index_read(
        &mut self,
        name: &str,
        index: &Expr,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let soa = self
            .soa_layouts
            .get(name)
            .cloned()
            .ok_or_else(|| format!("'{}' is not a SoA-laid-out collection", name))?;
        let slot = self
            .variables
            .get(name)
            .copied()
            .ok_or_else(|| format!("Undefined SoA variable '{}' in index expression", name))?;

        let has_cold = soa.cold_group.is_some();
        let soa_ty = self.soa_vec_type(soa.num_groups, has_cold);
        let len_idx = Self::soa_len_index(soa.num_groups, has_cold);
        let i64_t = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());

        let elem_struct_ty = *self
            .struct_types
            .get(&soa.struct_name)
            .ok_or_else(|| format!("Unknown SoA element struct '{}'", soa.struct_name))?;

        // Bounds check against len: panic if idx >= len.
        let idx_val = self.compile_expr(index)?.into_int_value();
        let len_ptr = self
            .builder
            .build_struct_gep(soa_ty, slot.ptr, len_idx, "soa.len.ptr")
            .unwrap();
        let len = self
            .builder
            .build_load(i64_t, len_ptr, "soa.len")
            .unwrap()
            .into_int_value();
        let fn_val = self.current_fn.unwrap();
        let oob_bb = self.context.append_basic_block(fn_val, "soa.idx.oob");
        let ok_bb = self.context.append_basic_block(fn_val, "soa.idx.ok");
        let oob = self
            .builder
            .build_int_compare(inkwell::IntPredicate::UGE, idx_val, len, "soa.bounds")
            .unwrap();
        self.builder
            .build_conditional_branch(oob, oob_bb, ok_bb)
            .unwrap();
        self.builder.position_at_end(oob_bb);
        self.emit_panic("index out of bounds");
        self.builder.build_unreachable().unwrap();
        self.builder.position_at_end(ok_bb);

        // Materialize the element struct: for each group, load its sub-
        // struct at [idx] and scatter the fields back to their original
        // positions in the element struct.
        let mut elem_val = elem_struct_ty.get_undef();
        let scatter_group = |this: &mut Self,
                             elem_val: &mut inkwell::values::StructValue<'ctx>,
                             struct_field_idx: u32,
                             group: &SoaGroup,
                             tag: &str| {
            let group_elem_ty = this.soa_group_elem_type(&soa.struct_name, group);
            let grp_ptr_ptr = this
                .builder
                .build_struct_gep(soa_ty, slot.ptr, struct_field_idx, &format!("{}.ptr", tag))
                .unwrap();
            let grp_buf = this
                .builder
                .build_load(ptr_ty, grp_ptr_ptr, &format!("{}.buf", tag))
                .unwrap()
                .into_pointer_value();
            let src = unsafe {
                this.builder
                    .build_gep(group_elem_ty, grp_buf, &[idx_val], &format!("{}.src", tag))
                    .unwrap()
            };
            let grp_val = this
                .builder
                .build_load(group_elem_ty, src, &format!("{}.val", tag))
                .unwrap()
                .into_struct_value();
            for (fi, &dst_idx) in group.field_indices.iter().enumerate() {
                let field_val = this
                    .builder
                    .build_extract_value(grp_val, fi as u32, "gf")
                    .unwrap();
                *elem_val = this
                    .builder
                    .build_insert_value(*elem_val, field_val, dst_idx as u32, "ef")
                    .unwrap()
                    .into_struct_value();
            }
        };

        // Hot groups: struct field index == group index.
        let hot_groups = soa.groups.clone();
        for (gi, group) in hot_groups.iter().enumerate() {
            scatter_group(self, &mut elem_val, gi as u32, group, &format!("g{}", gi));
        }
        // Cold group: pointer is last, after all hot groups.
        if let Some(cold) = soa.cold_group.clone() {
            let cold_idx = Self::soa_cold_ptr_index(soa.num_groups);
            scatter_group(self, &mut elem_val, cold_idx, &cold, "cold");
        }

        Ok(elem_val.into())
    }

    /// Compute the pointer to `vec_var[index]`'s element slot (the GEP
    /// into the Vec's heap buffer), with the same bounds-check elision as
    /// `compile_vec_index` — but WITHOUT the trailing load. Callers that
    /// want the element value use `compile_vec_index`; callers that need a
    /// borrow of the element (passing `vec[i]` to a `ref T` parameter)
    /// use this so the element is aliased in place rather than shallow-
    /// copied. Shallow-copying an aggregate element (Vec / String /
    /// heap struct) and then dropping the copy as a call-temp would
    /// double-free the buffer the outer Vec still owns.
    pub(super) fn vec_index_elem_ptr(
        &mut self,
        name: &str,
        index: &Expr,
    ) -> Result<inkwell::values::PointerValue<'ctx>, String> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let vec_ty = self.vec_struct_type();
        let elem_ty = self.vec_elem_type_for_var(name);

        let vec_ptr = self
            .get_data_ptr(name)
            .ok_or_else(|| format!("Undefined Vec variable '{}' in index expression", name))?;
        // Source-level elision: if the index is a bare identifier whose
        // bounds are proven by a dominating loop guard (recorded in
        // `asserted_index_bounds`), drop the matching half(s) of the
        // bounds check. Captured here BEFORE compiling the index so we
        // don't pay for the lookup when it can't fire (compound indices,
        // method-call indices, etc. immediately default to no elision).
        let (lower_proven, upper_proven) = self.index_bounds_already_proven(index, name);

        let idx_val = self.compile_expr(index)?.into_int_value();

        let data_pp = self
            .builder
            .build_struct_gep(vec_ty, vec_ptr, 0, "v.data.ptr")
            .unwrap();
        let data = self
            .builder
            .build_load(ptr_ty, data_pp, "v.data")
            .unwrap()
            .into_pointer_value();

        // Emit whichever halves of the bounds check the source-level
        // analysis didn't prove. The runtime panic path is reachable iff
        // some unproven half fails; both halves proven → no runtime
        // check at all (status quo for `unsafe { v.get_unchecked(i) }`).
        self.emit_split_bounds_check("vidx", idx_val, vec_ty, vec_ptr, lower_proven, upper_proven);

        let elem_ptr = unsafe {
            self.builder
                .build_gep(elem_ty, data, &[idx_val], "v.elem.ptr")
                .unwrap()
        };
        Ok(elem_ptr)
    }

    /// If `arg` is `vec_var[idx]` where `vec_var` is a plain Vec variable,
    /// return a pointer to the element slot in place — the borrow a `ref T`
    /// parameter wants. Returns `None` for any other shape (the caller then
    /// falls through to its rvalue-materialization path). Routing aggregate
    /// element borrows (`stake[i]` of `Vec[Vec[T]]`) through here instead of
    /// the load-then-track-for-drop path is what prevents the double-free:
    /// the element stays owned by the outer Vec, and no call-temp drop frees
    /// its still-shared buffer.
    pub(super) fn ref_arg_index_borrow_ptr(
        &mut self,
        arg: &Expr,
    ) -> Result<Option<inkwell::values::PointerValue<'ctx>>, String> {
        if let ExprKind::Index { object, index } = &arg.kind {
            if let ExprKind::Identifier(vec_var) = &object.kind {
                // Plain Vec variables only — slices / maps / array-slot
                // bindings have their own representations (mirror the
                // detection + array-slot bypass in `compile_index`).
                if self.vec_elem_types.contains_key(vec_var.as_str()) {
                    let slot_is_array = self
                        .variables
                        .get(vec_var.as_str())
                        .is_some_and(|s| matches!(s.ty, BasicTypeEnum::ArrayType(_)));
                    if !slot_is_array {
                        let ptr = self.vec_index_elem_ptr(vec_var, index)?;
                        return Ok(Some(ptr));
                    }
                }
            }
        }
        Ok(None)
    }

    /// Decide whether the dominating loop guard already proves either half
    /// of a `vec_var[idx]` safety check. Returns `(lower_proven, upper_proven)`:
    /// `lower_proven` ⇒ `idx >= 0` known; the negative-idx half can be
    /// dropped. `upper_proven` ⇒ `idx < vec_var.len()` known; the
    /// out-of-range half can be dropped.
    ///
    /// Only fires for bare-identifier indices (`v[i]`, never `v[i + 1]`).
    /// The kata's `chars[lo]` / `chars[hi]` shape passes; compound forms
    /// fall through to the full runtime check. Tightening to handle
    /// `v[i ± k]` for small known k is a follow-up; many real workloads
    /// don't need it (e.g. iterator-driven loops use bare-identifier
    /// indices), and the conservative default just means "no elision",
    /// not "wrong".
    pub(super) fn index_bounds_already_proven(&self, index: &Expr, vec_var: &str) -> (bool, bool) {
        let idx_name = match &index.kind {
            ExprKind::Identifier(name) => name.as_str(),
            _ => return (false, false),
        };
        let mut lower = false;
        let mut upper = false;
        for fact in &self.asserted_index_bounds {
            match fact {
                AssertedIndexBound::LowerBound { idx_var } if idx_var == idx_name => {
                    lower = true;
                }
                AssertedIndexBound::UpperBound {
                    idx_var,
                    vec_var: bound_vec,
                } if idx_var == idx_name && bound_vec == vec_var => {
                    upper = true;
                }
                _ => {}
            }
        }
        (lower, upper)
    }

    /// Emit the runtime bounds check for `vec_ptr[idx]`, dropping
    /// whichever half(s) the caller's `lower_proven` / `upper_proven`
    /// flags say are already established. The remaining branches still
    /// route OOB cases through `emit_panic("vec index out of bounds")`
    /// for safety; only the redundant compares are elided.
    ///
    /// When both halves are proven, this emits no bounds-check code at
    /// all — the caller's GEP+load runs straight through, matching the
    /// shape of `Vec.get_unchecked` for safe code that the source-level
    /// guard already justifies.
    pub(super) fn emit_split_bounds_check(
        &mut self,
        label_prefix: &str,
        idx_val: inkwell::values::IntValue<'ctx>,
        vec_ty: StructType<'ctx>,
        vec_ptr: PointerValue<'ctx>,
        lower_proven: bool,
        upper_proven: bool,
    ) {
        let i64_t = self.context.i64_type();
        let fn_val = self.current_fn.unwrap();

        // No check at all — both halves are pre-proven. Saves the load of
        // len and any branch / panic-block emission.
        if lower_proven && upper_proven {
            return;
        }

        // Neither half proven — fall back to the original single unsigned
        // bounds check. `icmp uge idx, len` catches both negative-idx (which
        // wraps to a huge unsigned value > any reasonable len) and
        // idx >= len in one compare + branch. Splitting into signed lower +
        // signed upper here would add an instruction without any elision
        // benefit (regression measured on kata-88's `nums1[k]` indexing,
        // where neither bound is asserted by the source guards).
        if !lower_proven && !upper_proven {
            let len_ptr = self
                .builder
                .build_struct_gep(vec_ty, vec_ptr, 1, "v.len.ptr")
                .unwrap();
            let len = self
                .builder
                .build_load(i64_t, len_ptr, "v.len")
                .unwrap()
                .into_int_value();
            let oob_bb = self
                .context
                .append_basic_block(fn_val, &format!("{label_prefix}.oob"));
            let ok_bb = self
                .context
                .append_basic_block(fn_val, &format!("{label_prefix}.ok"));
            let cmp = self
                .builder
                .build_int_compare(inkwell::IntPredicate::UGE, idx_val, len, "bounds")
                .unwrap();
            self.builder
                .build_conditional_branch(cmp, oob_bb, ok_bb)
                .unwrap();
            self.builder.position_at_end(oob_bb);
            self.emit_panic("vec index out of bounds");
            self.builder.build_unreachable().unwrap();
            self.builder.position_at_end(ok_bb);
            return;
        }

        // Lower-bound half: `idx < 0`. Skipped when the guard proved
        // `idx >= 0`; the load of `len` below is then loop-invariant
        // and LLVM will likely hoist it if both halves are emitted but
        // only the upper one is reached on the hot path.
        if !lower_proven {
            let oob_bb = self
                .context
                .append_basic_block(fn_val, &format!("{label_prefix}.oob.neg"));
            let ok_bb = self
                .context
                .append_basic_block(fn_val, &format!("{label_prefix}.lower.ok"));
            let neg = self
                .builder
                .build_int_compare(
                    inkwell::IntPredicate::SLT,
                    idx_val,
                    i64_t.const_zero(),
                    "bounds.neg",
                )
                .unwrap();
            self.builder
                .build_conditional_branch(neg, oob_bb, ok_bb)
                .unwrap();
            self.builder.position_at_end(oob_bb);
            self.emit_panic("vec index out of bounds");
            self.builder.build_unreachable().unwrap();
            self.builder.position_at_end(ok_bb);
        }

        // Upper-bound half: `idx >= len`. Skipped when the guard proved
        // `idx < vec_var.len()`. The signed `sge` predicate matches the
        // source-level signed loop guard's `slt` — LLVM's instcombine
        // folds the per-iteration redundant compare with the loop guard's
        // back-edge cmp when both have the same operands and predicate
        // family, which is the structural fix the `llvm.assume` spike
        // failed to trigger.
        if !upper_proven {
            let len_ptr = self
                .builder
                .build_struct_gep(vec_ty, vec_ptr, 1, "v.len.ptr")
                .unwrap();
            let len = self
                .builder
                .build_load(i64_t, len_ptr, "v.len")
                .unwrap()
                .into_int_value();
            let oob_bb = self
                .context
                .append_basic_block(fn_val, &format!("{label_prefix}.oob.upper"));
            let ok_bb = self
                .context
                .append_basic_block(fn_val, &format!("{label_prefix}.upper.ok"));
            let upper = if lower_proven {
                // Guard proved `idx >= 0`, so `idx u>= len` is equivalent
                // to `idx s>= len`. Use the signed form to match the
                // loop guard's predicate family for CSE.
                self.builder
                    .build_int_compare(inkwell::IntPredicate::SGE, idx_val, len, "bounds.upper")
                    .unwrap()
            } else {
                // Unreachable per the early-return above, but keep the
                // arm sound in case the caller's logic changes.
                self.builder
                    .build_int_compare(inkwell::IntPredicate::UGE, idx_val, len, "bounds")
                    .unwrap()
            };
            self.builder
                .build_conditional_branch(upper, oob_bb, ok_bb)
                .unwrap();
            self.builder.position_at_end(oob_bb);
            self.emit_panic("vec index out of bounds");
            self.builder.build_unreachable().unwrap();
            self.builder.position_at_end(ok_bb);
        }
    }

    pub(super) fn compile_vec_index_store(
        &mut self,
        var_name: &str,
        index: &Expr,
        val: BasicValueEnum<'ctx>,
    ) -> Result<(), String> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let vec_ty = self.vec_struct_type();
        let elem_ty = self.vec_elem_type_for_var(var_name);
        let vec_ptr = self
            .get_data_ptr(var_name)
            .ok_or_else(|| format!("Undefined Vec variable '{}' in index store", var_name))?;
        let (lower_proven, upper_proven) = self.index_bounds_already_proven(index, var_name);
        let idx_val = self.compile_expr(index)?.into_int_value();

        let data_pp = self
            .builder
            .build_struct_gep(vec_ty, vec_ptr, 0, "v.st.data.ptr")
            .unwrap();
        let data = self
            .builder
            .build_load(ptr_ty, data_pp, "v.st.data")
            .unwrap()
            .into_pointer_value();

        self.emit_split_bounds_check("v.st", idx_val, vec_ty, vec_ptr, lower_proven, upper_proven);
        let elem_ptr = unsafe {
            self.builder
                .build_gep(elem_ty, data, &[idx_val], "v.st.elem.ptr")
                .unwrap()
        };
        // Narrow to the element width before storing — a computed scalar for a
        // sub-word element (`v[i] = b'a' + (k as u8)` into `Vec[u8]`) compiles
        // to i64 and would write 8 bytes over a 1-byte slot. Same fix as the
        // `push` element store and `coerce_to_struct_field_ty`.
        let val = self.coerce_scalar_to_type(val, elem_ty);
        self.builder.build_store(elem_ptr, val).unwrap();
        Ok(())
    }

    /// `outer[oi][ii] = val` for `outer: Vec[Vec[T]]`. The outer indexed
    /// expression is a Vec[T] aggregate (24-byte `{ptr, len, cap}`) living
    /// inside `outer.data`; we GEP to its address (not a load — we want
    /// an L-value), pick up the inner data pointer, GEP into it by
    /// `ii`, and store. Bounds checks on both indices use the same
    /// `emit_split_bounds_check` helper as the single-level path.
    ///
    /// Without this arm, `compile_index_store` falls through to its
    /// "Index assignment target must be a variable" error, forcing
    /// users into a flat-layout workaround: a single `Vec[T]` of size
    /// `outer*len` with the natural `rows[cur][end]` write rewritten
    /// by hand as `rows_flat[cur * len + end] = X`.
    pub(super) fn compile_nested_vec_vec_index_store(
        &mut self,
        outer_name: &str,
        outer_index: &Expr,
        inner_index: &Expr,
        val: BasicValueEnum<'ctx>,
    ) -> Result<(), String> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let vec_ty = self.vec_struct_type();
        let outer_elem_ty = self.vec_elem_type_for_var(outer_name);
        let outer_vec_ptr = self.get_data_ptr(outer_name).ok_or_else(|| {
            format!(
                "Undefined Vec variable '{}' in nested index store",
                outer_name
            )
        })?;

        // Inner element type comes from the outer binding's stored
        // TypeExpr — for `rows: Vec[Vec[i64]]`, var_elem_type_exprs
        // holds `Vec[i64]`, from which we extract `i64`. If the outer
        // element isn't itself a Vec (e.g., `rows: Vec[i64]`, in which
        // case `rows[i][j]` is a typecheck error anyway), this returns
        // an error so compilation surfaces the misuse instead of
        // producing a wild GEP.
        let inner_elem_te = self
            .var_elem_type_exprs
            .get(outer_name)
            .and_then(vec_inner_type_expr)
            .ok_or_else(|| {
                format!(
                    "Nested index store: outer variable '{outer_name}' is not a Vec[Vec[T]] — \
                     element type isn't itself a Vec"
                )
            })?;
        let inner_elem_ty = self.llvm_type_for_type_expr(&inner_elem_te);

        // Outer GEP: outer_data + oi * sizeof(Vec_struct) → pointer
        // to the inner Vec aggregate.
        let (outer_lo, outer_hi) = self.index_bounds_already_proven(outer_index, outer_name);
        let oi = self.compile_expr(outer_index)?.into_int_value();
        let outer_data_pp = self
            .builder
            .build_struct_gep(vec_ty, outer_vec_ptr, 0, "nvv.outer.data.pp")
            .unwrap();
        let outer_data = self
            .builder
            .build_load(ptr_ty, outer_data_pp, "nvv.outer.data")
            .unwrap()
            .into_pointer_value();
        self.emit_split_bounds_check("nvv.outer", oi, vec_ty, outer_vec_ptr, outer_lo, outer_hi);
        let inner_vec_ptr = unsafe {
            self.builder
                .build_gep(outer_elem_ty, outer_data, &[oi], "nvv.inner.vec.ptr")
                .unwrap()
        };

        // Inner GEP: load the inner Vec's data field, then `data + ii`.
        // The inner bounds-check reads .len from the inner Vec
        // aggregate via `emit_split_bounds_check`'s struct_gep on
        // field 1 (vec_ty layout) — works because the inner aggregate
        // is laid out exactly like an outer Vec, just embedded.
        let ii = self.compile_expr(inner_index)?.into_int_value();
        let inner_data_pp = self
            .builder
            .build_struct_gep(vec_ty, inner_vec_ptr, 0, "nvv.inner.data.pp")
            .unwrap();
        let inner_data = self
            .builder
            .build_load(ptr_ty, inner_data_pp, "nvv.inner.data")
            .unwrap()
            .into_pointer_value();
        self.emit_split_bounds_check("nvv.inner", ii, vec_ty, inner_vec_ptr, false, false);
        let leaf_ptr = unsafe {
            self.builder
                .build_gep(inner_elem_ty, inner_data, &[ii], "nvv.leaf.ptr")
                .unwrap()
        };
        self.builder.build_store(leaf_ptr, val).unwrap();
        Ok(())
    }

    pub(super) fn compile_slice_index_store(
        &mut self,
        var_name: &str,
        index: &Expr,
        val: BasicValueEnum<'ctx>,
    ) -> Result<(), String> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let slice_ty = self.slice_struct_type();
        let elem_ty = *self.slice_elem_types.get(var_name).unwrap();
        let slice_ptr = self.get_data_ptr(var_name).unwrap();
        let (lower_proven, upper_proven) = self.index_bounds_already_proven(index, var_name);
        let idx_val = self.compile_expr(index)?.into_int_value();

        let data_pp = self
            .builder
            .build_struct_gep(slice_ty, slice_ptr, 0, "s.st.data.pp")
            .unwrap();
        let data = self
            .builder
            .build_load(ptr_ty, data_pp, "s.st.data")
            .unwrap()
            .into_pointer_value();

        // Slice layout `{ptr, i64}` has len at field 1, same offset as
        // Vec's `{ptr, i64, i64}` — the helper's struct-gep only touches
        // field 1, so passing slice_ty is sound. The OOB diagnostic is
        // shared with Vec (`vec index out of bounds`) per the kata-5
        // precedent; users routing through `Slice.get` get the typed
        // diagnostic via the safe path, this is the unsafe-form panic.
        self.emit_split_bounds_check(
            "s.st",
            idx_val,
            slice_ty,
            slice_ptr,
            lower_proven,
            upper_proven,
        );
        let elem_ptr = unsafe {
            self.builder
                .build_gep(elem_ty, data, &[idx_val], "s.st.elem.ptr")
                .unwrap()
        };
        // Narrow to element width before storing into a sub-word slice element
        // — same fix as the Vec push / index-store paths.
        let val = self.coerce_scalar_to_type(val, elem_ty);
        self.builder.build_store(elem_ptr, val).unwrap();
        Ok(())
    }

    pub(super) fn compile_slice_index(
        &mut self,
        var_name: &str,
        index: &Expr,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let slice_ty = self.slice_struct_type();
        let elem_ty = *self.slice_elem_types.get(var_name).unwrap();
        let slice_ptr = self.get_data_ptr(var_name).unwrap();
        // Source-level elision: bare-identifier index whose bounds are
        // proven by an enclosing while-guard / short-circuit `and` skips
        // the matching half of the runtime check. Mirrors the Vec read
        // path. Captured before compiling the index so compound-index
        // shapes (`v[i + 1]`) drop straight to (false, false) — the
        // index-name match in `index_bounds_already_proven` requires a
        // bare `Identifier` source-level node.
        let (lower_proven, upper_proven) = self.index_bounds_already_proven(index, var_name);
        let idx_val = self.compile_expr(index)?.into_int_value();

        let data_pp = self
            .builder
            .build_struct_gep(slice_ty, slice_ptr, 0, "s.data.pp")
            .unwrap();
        let data = self
            .builder
            .build_load(ptr_ty, data_pp, "s.data")
            .unwrap()
            .into_pointer_value();

        self.emit_split_bounds_check(
            "sidx",
            idx_val,
            slice_ty,
            slice_ptr,
            lower_proven,
            upper_proven,
        );
        let elem_ptr = unsafe {
            self.builder
                .build_gep(elem_ty, data, &[idx_val], "s.elem.ptr")
                .unwrap()
        };
        let val = self
            .builder
            .build_load(elem_ty, elem_ptr, "s.elem")
            .unwrap();
        Ok(val)
    }

    pub(super) fn compile_index_store(
        &mut self,
        object: &Expr,
        index: &Expr,
        val: BasicValueEnum<'ctx>,
    ) -> Result<(), String> {
        // Tensor element store: `t[i, j] = v` — same layout helpers as
        // the read path (`src/codegen/tensor.rs`).
        if let ExprKind::Identifier(name) = &object.kind {
            if let Some(info) = self.tensor_var_infos.get(name.as_str()).cloned() {
                let t_ptr = self.tensor_ptr_for_var(name)?;
                return self.compile_tensor_index_store(t_ptr, &info, index, val);
            }
        }

        // Slice[T] / mut Slice[T] element store: the slice is a `{ptr, i64}`
        // value; the index path GEPs through the stored data pointer. The
        // ownership checker is responsible for rejecting stores through a
        // read-only Slice[T] — codegen treats the write path uniformly.
        if let ExprKind::Identifier(name) = &object.kind {
            if self.slice_elem_types.contains_key(name.as_str()) {
                return self.compile_slice_index_store(name, index, val);
            }
        }

        // Map[K, V] element store: `m[k] = v` lowers to karac_map_insert_old
        // discarding the previous-value out-slot. Fresh-insert and overwrite
        // are both handled by the same runtime call.
        if let ExprKind::Identifier(name) = &object.kind {
            if self.map_key_types.contains_key(name.as_str()) {
                return self.compile_map_index_store(name, index, val);
            }
        }

        // Vec[T] element store: bounds-check against `len` (not `cap`) and
        // GEP `data[i]`. Mirrors the read-path in `compile_vec_index`.
        //
        // Same ArrayType-slot guard as compile_index: when the typechecker
        // registered "Vec" for a binding (synthesis-mode bare ArrayLiteral)
        // but the alloca is sized as `[N x T]`, the Vec dispatch produces
        // wild GEPs. Fall through to the Array path below.
        if let ExprKind::Identifier(name) = &object.kind {
            if self.vec_elem_types.contains_key(name.as_str()) {
                let slot_is_array = self
                    .variables
                    .get(name.as_str())
                    .is_some_and(|s| matches!(s.ty, BasicTypeEnum::ArrayType(_)));
                if !slot_is_array {
                    return self.compile_vec_index_store(name, index, val);
                }
            }
        }

        // Nested indexed assignment: `outer[oi][ii] = val` where outer
        // is a named Vec[Vec[T]] binding. Dispatched before the
        // "must be a variable" gate below — without this arm the user
        // is forced into a flat-layout workaround
        // (single Vec[T] of size outer*len with hand-computed strides).
        if let ExprKind::Index {
            object: outer,
            index: outer_idx,
        } = &object.kind
        {
            if let ExprKind::Identifier(outer_name) = &outer.kind {
                let outer_is_vec_of_vec = self
                    .var_elem_type_exprs
                    .get(outer_name.as_str())
                    .and_then(vec_inner_type_expr)
                    .is_some();
                if outer_is_vec_of_vec {
                    return self
                        .compile_nested_vec_vec_index_store(outer_name, outer_idx, index, val);
                }
            }
        }

        // Field-access-rooted index store (`h.items[i] = v`,
        // `node.neighbors[i] = n`, incl. shared-struct receivers):
        // resolve the field's storage pointer via the FR-slice helper,
        // mint a synth identifier, and recurse — exact mirror of
        // `compile_index`'s FieldAccess arm on the read side. Without
        // this arm the store falls to the "must be a variable" gate
        // below. (The interpreter had the same write gap, silently
        // no-op'ing the store — fixed in the same slice.)
        if let ExprKind::FieldAccess {
            object: inner,
            field,
        } = &object.kind
        {
            // `self.field[i] = v` — `self` parses as `SelfValue`, which
            // `lower_field_access_ptr` deliberately returns `Ok(None)` for
            // (so `self.field.method()` dispatch keeps falling through). Mirror
            // `compile_index`'s read-side normalisation: rewrite the receiver
            // to a synthetic `Identifier("self")` so the Identifier arm
            // resolves it. Without this the store dies on the "must be a
            // variable" gate below for every `self.field[i] = v`.
            let self_ident;
            let inner: &Expr = if matches!(inner.kind, ExprKind::SelfValue) {
                self_ident = Expr {
                    kind: ExprKind::Identifier("self".to_string()),
                    span: inner.span.clone(),
                };
                &self_ident
            } else {
                inner
            };
            if let Some((field_ptr, field_ll_ty, field_te)) =
                self.lower_field_access_ptr(inner, field, "index-store expression")?
            {
                let synth = format!("__field_elem_{}", self.indexed_elem_counter);
                self.indexed_elem_counter += 1;
                self.variables.insert(
                    synth.clone(),
                    super::state::VarSlot {
                        ptr: field_ptr,
                        ty: field_ll_ty,
                    },
                );
                self.register_var_from_type_expr(&synth, &field_te);
                let synth_expr = Expr {
                    kind: ExprKind::Identifier(synth.clone()),
                    span: object.span.clone(),
                };
                let result = self.compile_index_store(&synth_expr, index, val);

                // Clean up synth registrations (same set as the FR slice).
                self.variables.remove(&synth);
                self.vec_elem_types.remove(&synth);
                self.slice_elem_types.remove(&synth);
                self.var_elem_type_exprs.remove(&synth);
                self.var_type_names.remove(&synth);
                self.map_key_types.remove(&synth);
                self.map_val_types.remove(&synth);
                self.map_key_type_names.remove(&synth);
                self.map_key_type_exprs.remove(&synth);
                self.set_elem_types.remove(&synth);
                self.set_elem_type_names.remove(&synth);
                self.set_elem_type_exprs.remove(&synth);

                return result;
            }
        }

        let idx_val = self.compile_expr(index)?.into_int_value();
        let i64_t = self.context.i64_type();

        let (arr_ptr, arr_ty) = if let ExprKind::Identifier(name) = &object.kind {
            if let Some((data_ptr, ref_arr_ty)) = self.ref_array_index_target(name) {
                (data_ptr, ref_arr_ty)
            } else if let Some(slot) = self.variables.get(name.as_str()).copied() {
                (slot.ptr, slot.ty)
            } else {
                return Err(format!("Undefined variable '{}' in index store", name));
            }
        } else {
            return Err("Index assignment target must be a variable".to_string());
        };

        if let BasicTypeEnum::ArrayType(at) = arr_ty {
            let len = i64_t.const_int(at.len() as u64, false);
            let fn_val = self.current_fn.unwrap();
            let oob_bb = self.context.append_basic_block(fn_val, "idx_s.oob");
            let ok_bb = self.context.append_basic_block(fn_val, "idx_s.ok");
            let cmp = self
                .builder
                .build_int_compare(inkwell::IntPredicate::UGE, idx_val, len, "bounds")
                .unwrap();
            self.builder
                .build_conditional_branch(cmp, oob_bb, ok_bb)
                .unwrap();

            self.builder.position_at_end(oob_bb);
            self.emit_panic("array index out of bounds");
            self.builder.build_unreachable().unwrap();

            self.builder.position_at_end(ok_bb);
            let zero = i64_t.const_int(0, false);
            let elem_ptr = unsafe {
                self.builder
                    .build_gep(arr_ty, arr_ptr, &[zero, idx_val], "arr.store.ptr")
                    .unwrap()
            };
            self.builder.build_store(elem_ptr, val).unwrap();
            Ok(())
        } else {
            Err("Index store on non-array type".to_string())
        }
    }
}

/// Integer literal value of `e`, if `e` is a bare integer literal. Negative
/// literals parse as `Unary { Neg, Integer }`, not `Integer(<0)`, so a value
/// returned here is always `>= 0` for the forms slice bounds take — but callers
/// still range-check, never assuming sign.
fn slice_bound_int_lit(e: &Expr) -> Option<i64> {
    match e.kind {
        ExprKind::Integer(n, _) => Some(n),
        _ => None,
    }
}

/// Structural equality for the narrow class of side-effect-free, value-stable
/// expressions a slice bound can be: identifiers, dotted paths, `self`, integer
/// literals, and field / tuple-index projections over the same. Two such
/// expressions that are structurally equal read the *same value* when evaluated
/// twice with no intervening statements (as in `s[a..a+k]`), which is what makes
/// folding the width to `k` sound. Deliberately conservative — calls, indexing,
/// and arithmetic return `false`, so the caller keeps the exact runtime
/// subtraction. Must never return a false positive.
fn slice_bound_place_eq(a: &Expr, b: &Expr) -> bool {
    match (&a.kind, &b.kind) {
        (ExprKind::Identifier(x), ExprKind::Identifier(y)) => x == y,
        (ExprKind::Integer(x, _), ExprKind::Integer(y, _)) => x == y,
        (ExprKind::SelfValue, ExprKind::SelfValue) => true,
        (ExprKind::Path { segments: s1, .. }, ExprKind::Path { segments: s2, .. }) => s1 == s2,
        (
            ExprKind::FieldAccess {
                object: o1,
                field: f1,
            },
            ExprKind::FieldAccess {
                object: o2,
                field: f2,
            },
        ) => f1 == f2 && slice_bound_place_eq(o1, o2),
        (
            ExprKind::TupleIndex {
                object: o1,
                index: i1,
            },
            ExprKind::TupleIndex {
                object: o2,
                index: i2,
            },
        ) => i1 == i2 && slice_bound_place_eq(o1, o2),
        _ => false,
    }
}

/// Compile-time byte width of a borrowed string slice `s[start..end]` (or
/// `..=end`), when statically determinable and non-negative. Recognizes the
/// two fixed-width forms the hot `out.push_str(s[d..d+1])` builder loop emits:
///
///   * both bounds integer literals — `s[2..5]` ⇒ 3;
///   * `s[a..a+k]` / `s[a..=a+k]` — upper bound is the lower bound plus an
///     integer literal, with `a` a side-effect-free place (so the two reads
///     yield the same value) ⇒ `k` (`+1` inclusive).
///
/// Returns `None` for an open upper bound (`s[a..]`, whose width is the runtime
/// string length) and any runtime or unrecognized shape; the caller then falls
/// back to the exact `end - start` subtraction. The value equals `end - start`
/// by construction whenever it is `Some`, so substituting it for the runtime
/// computation is sound — the slice's runtime bounds validation is unaffected.
fn const_slice_width(
    start: &Option<Box<Expr>>,
    end: &Option<Box<Expr>>,
    inclusive: bool,
) -> Option<u64> {
    // An open upper bound resolves to the runtime string length.
    let end = end.as_ref()?;
    let incl: i64 = if inclusive { 1 } else { 0 };

    // Both bounds integer literals (absent lower bound = 0): width = end - start.
    let start_lit = match start {
        None => Some(0),
        Some(e) => slice_bound_int_lit(e),
    };
    if let (Some(a), Some(b)) = (start_lit, slice_bound_int_lit(end)) {
        return b
            .checked_add(incl)
            .and_then(|hi| hi.checked_sub(a))
            .and_then(|w| u64::try_from(w).ok());
    }

    // `s[a..a+k]`: upper bound is `a + k` (or `k + a`) with `a` structurally
    // equal to the lower bound. The width is the literal `k` (+1 inclusive).
    let start_e = start.as_ref()?;
    let (left, right) = as_addition(end)?;
    if let Some(k) = slice_bound_int_lit(right) {
        if slice_bound_place_eq(start_e, left) {
            return k.checked_add(incl).and_then(|w| u64::try_from(w).ok());
        }
    }
    if let Some(k) = slice_bound_int_lit(left) {
        if slice_bound_place_eq(start_e, right) {
            return k.checked_add(incl).and_then(|w| u64::try_from(w).ok());
        }
    }
    None
}

/// `(left, right)` of an addition expression, matching both the pre-lowered
/// `Binary { Add, .. }` AST and the post-lowering
/// `Call(Path([type, "add"]), [left, right])` form that `src/lowering.rs`
/// rewrites every primitive `+` into before codegen runs (so `s[d..d+1]` reaches
/// here as a call, not a `Binary`). The 2-segment `[type, "add"]` path is the
/// established boundary that separates the numeric intrinsic from a user-defined
/// `add` — mirrors `match_lowered_op_call` (concurrency.rs) and `as_binop`
/// (control_flow_bce.rs). Returns `None` for any other shape.
fn as_addition(e: &Expr) -> Option<(&Expr, &Expr)> {
    match &e.kind {
        ExprKind::Binary {
            op: BinOp::Add,
            left,
            right,
        } => Some((left, right)),
        ExprKind::Call { callee, args } => {
            let ExprKind::Path { segments, .. } = &callee.kind else {
                return None;
            };
            if segments.len() == 2 && segments[1] == "add" && args.len() == 2 {
                Some((&args[0].value, &args[1].value))
            } else {
                None
            }
        }
        _ => None,
    }
}
