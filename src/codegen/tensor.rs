//! `Tensor[T, Shape]` LLVM lowering (phase-11 numerical stdlib, core
//! slice). Mirrors the interpreter intrinsics in
//! `src/interpreter/method_call_tensor.rs`; typing is enforced upstream
//! by `src/typechecker/expr_method_tensor.rs` and the Tensor index arm
//! in `typechecker/exprs.rs`.
//!
//! ## Value layout
//!
//! A tensor value is a single pointer to one malloc'd block:
//!
//! ```text
//! offset 0:            i64 rank
//! offset 8:            i64 dims[0..rank]
//! offset 8*(1+rank):   data, C-order (row-major), element-typed
//! ```
//!
//! Rationale: the typechecker guarantees a statically-known rank at
//! every site that touches data (constructors, indexing, the
//! shape-transform family), so all data offsets fold to constants —
//! while `shape()` / `rank()` read the header and therefore work even
//! on splice-generic receivers. One pointer means trivial ABI (params,
//! returns, moves are pointer copies), one `free`, and a null-store
//! suppression sentinel for move-out sites (the analog of Vec's
//! `cap = 0`). The data region is contiguous C-order per the Arrow
//! commitment; the 64-byte-alignment recommendation is deferred to the
//! Arrow IPC slice where zero-copy actually needs it (malloc's 16-byte
//! alignment is valid for every element type shipped here).
//!
//! ## Static dims as an optimization, header as truth
//!
//! `TensorVarInfo.dims` carries `Some(n)` for dims that are concrete
//! literals in the static type — those fold into constant strides and
//! let literal-index bounds checks be elided (the typechecker already
//! proved them). `None` dims load from the header. The header is
//! written once at construction and is always authoritative; static
//! info never disagrees with it because construction asserts agreement
//! (the construction-boundary check of design.md § Runtime equality
//! check — `Tensor.zeros([7, 5])` bound to `Tensor[f64, [3, ?]]` traps
//! on dim 0).

use inkwell::types::BasicTypeEnum;
use inkwell::values::{BasicValueEnum, IntValue, PointerValue};
use inkwell::{AddressSpace, IntPredicate};

use crate::ast::{CallArg, Expr, ExprKind, GenericArg, ShapeDim, TypeExpr, TypeKind};

use super::state::TensorVarInfo;

impl<'ctx> super::Codegen<'ctx> {
    // ── Layout helpers ──────────────────────────────────────────

    /// GEP to the i64 dim slot `i` of the header (slot 0 is rank;
    /// dims start at slot 1).
    fn tensor_header_slot(
        &self,
        t_ptr: PointerValue<'ctx>,
        slot: u64,
        name: &str,
    ) -> PointerValue<'ctx> {
        let i64_t = self.context.i64_type();
        unsafe {
            self.builder
                .build_gep(i64_t, t_ptr, &[i64_t.const_int(slot, false)], name)
                .unwrap()
        }
    }

    /// Load dim `i` from the tensor header.
    fn tensor_load_dim(&self, t_ptr: PointerValue<'ctx>, i: usize) -> IntValue<'ctx> {
        let slot = self.tensor_header_slot(t_ptr, 1 + i as u64, &format!("t.dim{}.p", i));
        self.builder
            .build_load(self.context.i64_type(), slot, &format!("t.dim{}", i))
            .unwrap()
            .into_int_value()
    }

    /// Dim `i` as an IntValue — constant when statically known, header
    /// load otherwise.
    fn tensor_dim_value(
        &self,
        t_ptr: PointerValue<'ctx>,
        info: &TensorVarInfo<'ctx>,
        i: usize,
    ) -> IntValue<'ctx> {
        match info.dims[i] {
            Some(d) => self.context.i64_type().const_int(d as u64, false),
            None => self.tensor_load_dim(t_ptr, i),
        }
    }

    /// Pointer to the first data element. `rank` must be the static
    /// rank (data access sites are always splice-free).
    fn tensor_data_ptr(
        &self,
        t_ptr: PointerValue<'ctx>,
        rank: usize,
        name: &str,
    ) -> PointerValue<'ctx> {
        self.tensor_header_slot(t_ptr, 1 + rank as u64, name)
    }

    /// Lower a plain-data `TensorTypeInfo` (lowering side-table) to the
    /// codegen-side `TensorVarInfo`.
    pub(super) fn tensor_var_info_from_table(
        &self,
        ti: &crate::ast::TensorTypeInfo,
    ) -> TensorVarInfo<'ctx> {
        TensorVarInfo {
            elem: self.llvm_type_for_type_expr(&ti.elem),
            dims: ti.dims.clone(),
        }
    }

    /// Extract a `TensorVarInfo` from an annotation TypeExpr
    /// (`Tensor[T, [d0, d1, ...]]`). Returns `None` for non-Tensor
    /// types or splice-bearing shapes (rank unknown — registration
    /// skipped; `shape()`/`rank()` still work via the header).
    pub(super) fn tensor_var_info_from_type_expr(
        &self,
        te: &TypeExpr,
    ) -> Option<TensorVarInfo<'ctx>> {
        let TypeKind::Path(path) = &te.kind else {
            return None;
        };
        if path.segments.last().map(|s| s.as_str()) != Some("Tensor") {
            return None;
        }
        let gargs = path.generic_args.as_ref()?;
        let mut elem: Option<BasicTypeEnum<'ctx>> = None;
        let mut dims: Option<Vec<Option<i64>>> = None;
        for ga in gargs {
            match ga {
                GenericArg::Type(t) if elem.is_none() => {
                    elem = Some(self.llvm_type_for_type_expr(t));
                }
                GenericArg::Shape(shape) => {
                    let mut out = Vec::with_capacity(shape.dims.len());
                    for d in &shape.dims {
                        match d {
                            ShapeDim::Const(e) => match &e.kind {
                                ExprKind::Integer(v, _) => out.push(Some(*v)),
                                // Named dim param / const expr — runtime
                                // (read from the header).
                                _ => out.push(None),
                            },
                            ShapeDim::Dynamic { .. } => out.push(None),
                            ShapeDim::Splice { .. } => return None,
                        }
                    }
                    dims = Some(out);
                }
                _ => {}
            }
        }
        Some(TensorVarInfo {
            elem: elem?,
            dims: dims?,
        })
    }

    // ── Constructors ────────────────────────────────────────────

    /// `Tensor.zeros(dims)` / `Tensor.ones(dims)` / `Tensor.full(dims,
    /// value)` — element type and static rank come from the destination
    /// binding's annotation via `pending_let_tensor_info` (the
    /// `Vec.with_capacity` expected-type mechanism; the typechecker
    /// enforces the annotation upstream, so a missing pending here is a
    /// codegen-order bug, not a user error). The runtime dims argument
    /// is length-asserted against the static rank and value-asserted
    /// against every static dim — the construction-boundary check of
    /// design.md § Runtime equality check.
    pub(super) fn compile_tensor_new(
        &mut self,
        method: &str,
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let info = self.pending_let_tensor_info.clone().ok_or_else(|| {
            format!(
                "Tensor.{}: element type and rank unknown — requires a \
                 `let t: Tensor[T, [dims]] = ...` annotation",
                method
            )
        })?;
        let rank = info.dims.len();
        let i64_t = self.context.i64_type();

        // Evaluate the dims argument. Array-literal fast path: read the
        // entry expressions directly (no Vec materialization). General
        // path: compile to a Vec {ptr, len, cap}, assert len == rank,
        // load each entry, and eagerly free a temporary's buffer.
        let dim_vals: Vec<IntValue<'ctx>> = match args.first().map(|a| &a.value) {
            Some(Expr {
                kind: ExprKind::ArrayLiteral(entries),
                ..
            }) if entries.len() == rank => {
                let mut vals = Vec::with_capacity(rank);
                for e in entries {
                    vals.push(self.compile_expr(e)?.into_int_value());
                }
                vals
            }
            Some(arg_expr) => {
                let arg_val = self.compile_expr(arg_expr)?;
                // Bare array-literal bindings (`let dims = [2, 3];`)
                // compile to an `[N x i64]` aggregate even though the
                // typechecker types them Vec[i64] (the synthesis-mode
                // Array-slot shape compile_index also guards against).
                // Read the elements straight out of the aggregate.
                if let BasicValueEnum::ArrayValue(av) = arg_val {
                    let n = av.get_type().len() as usize;
                    if n != rank {
                        return Err(format!(
                            "Tensor.{}: dims list has {} entr{}, expected rank {}",
                            method,
                            n,
                            if n == 1 { "y" } else { "ies" },
                            rank
                        ));
                    }
                    let mut vals = Vec::with_capacity(rank);
                    for i in 0..rank {
                        vals.push(
                            self.builder
                                .build_extract_value(av, i as u32, &format!("t.dims.{}", i))
                                .unwrap()
                                .into_int_value(),
                        );
                    }
                    vals
                } else {
                    let vec_val = arg_val.into_struct_value();
                    let data = self
                        .builder
                        .build_extract_value(vec_val, 0, "t.dims.data")
                        .unwrap()
                        .into_pointer_value();
                    let len = self
                        .builder
                        .build_extract_value(vec_val, 1, "t.dims.len")
                        .unwrap()
                        .into_int_value();
                    // len == rank assert.
                    let want = i64_t.const_int(rank as u64, false);
                    let ok = self
                        .builder
                        .build_int_compare(IntPredicate::EQ, len, want, "t.rank.ok")
                        .unwrap();
                    self.emit_tensor_guard(
                        ok,
                        &format!(
                            "Tensor.{}: dims list length does not match the \
                         annotated rank {}",
                            method, rank
                        ),
                    )?;
                    let mut vals = Vec::with_capacity(rank);
                    for i in 0..rank {
                        let slot = unsafe {
                            self.builder
                                .build_gep(
                                    i64_t,
                                    data,
                                    &[i64_t.const_int(i as u64, false)],
                                    &format!("t.dims.{}p", i),
                                )
                                .unwrap()
                        };
                        vals.push(
                            self.builder
                                .build_load(i64_t, slot, &format!("t.dims.{}", i))
                                .unwrap()
                                .into_int_value(),
                        );
                    }
                    // Eagerly free a temporary dims Vec (non-identifier
                    // arg) — nothing else owns its buffer. Identifier
                    // args keep their own scope cleanup.
                    if !matches!(arg_expr.kind, ExprKind::Identifier(_)) {
                        let cap = self
                            .builder
                            .build_extract_value(vec_val, 2, "t.dims.cap")
                            .unwrap()
                            .into_int_value();
                        self.emit_free_if_cap_positive(data, cap);
                    }
                    vals
                }
            }
            None => return Err(format!("Tensor.{}: missing dims argument", method)),
        };

        // Static-dim agreement asserts (construction boundary).
        for (i, (dim_val, static_dim)) in dim_vals.iter().zip(info.dims.iter()).enumerate() {
            if let Some(d) = static_dim {
                let want = i64_t.const_int(*d as u64, false);
                let ok = self
                    .builder
                    .build_int_compare(IntPredicate::EQ, *dim_val, want, "t.dim.ok")
                    .unwrap();
                self.emit_tensor_guard(
                    ok,
                    &format!(
                        "Tensor.{}: runtime dim {} does not match the \
                         annotated static dim (expected {})",
                        method, i, d
                    ),
                )?;
            }
        }

        // count = product(dims); bytes = 8*(1+rank) + count*elem_size.
        let mut count = i64_t.const_int(1, false);
        for dv in &dim_vals {
            count = self.builder.build_int_mul(count, *dv, "t.count").unwrap();
        }
        let elem_size = self.tensor_elem_size(info.elem)?;
        let data_bytes = self
            .builder
            .build_int_mul(count, i64_t.const_int(elem_size, false), "t.data.bytes")
            .unwrap();
        let header_bytes = i64_t.const_int(8 * (1 + rank as u64), false);
        let total = self
            .builder
            .build_int_add(header_bytes, data_bytes, "t.bytes")
            .unwrap();
        let t_ptr = self
            .builder
            .build_call(self.malloc_fn, &[total.into()], "t.alloc")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();

        // Header: rank + dims.
        let rank_slot = self.tensor_header_slot(t_ptr, 0, "t.rank.p");
        self.builder
            .build_store(rank_slot, i64_t.const_int(rank as u64, false))
            .unwrap();
        for (i, dv) in dim_vals.iter().enumerate() {
            let slot = self.tensor_header_slot(t_ptr, 1 + i as u64, &format!("t.d{}.p", i));
            self.builder.build_store(slot, *dv).unwrap();
        }

        // Fill.
        let data = self.tensor_data_ptr(t_ptr, rank, "t.data");
        match method {
            "zeros" => {
                // All-zero bit patterns are correct for ints, floats
                // (+0.0), and bool alike.
                self.builder
                    .build_memset(data, 8, self.context.i8_type().const_zero(), data_bytes)
                    .map_err(|e| format!("Tensor.zeros memset failed: {:?}", e))?;
            }
            "ones" | "full" => {
                let fill: BasicValueEnum<'ctx> = if method == "ones" {
                    match info.elem {
                        BasicTypeEnum::FloatType(ft) => ft.const_float(1.0).into(),
                        BasicTypeEnum::IntType(it) => it.const_int(1, false).into(),
                        other => {
                            return Err(format!(
                                "Tensor.ones: unsupported element type {:?}",
                                other
                            ))
                        }
                    }
                } else {
                    let val_arg = args
                        .get(1)
                        .ok_or_else(|| "Tensor.full: missing value argument".to_string())?;
                    self.compile_expr(&val_arg.value)?
                };
                self.emit_tensor_fill_loop(data, info.elem, count, fill);
            }
            other => return Err(format!("unknown Tensor constructor '{}'", other)),
        }
        Ok(t_ptr.into())
    }

    /// `Tensor.from(nested array literal)` — fully self-contained:
    /// dims come from the literal's syntactic nesting (the typechecker
    /// already validated raggedness) and the element type from the
    /// first leaf's compiled value (the typechecker's
    /// `infer_tensor_from` synthesizes the element type from the same
    /// first leaf and checks every other leaf and any annotation
    /// against it, so leaf-driven is exact — no expected-type
    /// threading needed).
    pub(super) fn compile_tensor_from(
        &mut self,
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let data_expr = args
            .first()
            .map(|a| &a.value)
            .ok_or_else(|| "Tensor.from: missing literal argument".to_string())?;
        let mut dims: Vec<i64> = Vec::new();
        let mut leaves: Vec<&Expr> = Vec::new();
        collect_tensor_literal(data_expr, 0, &mut dims, &mut leaves)
            .map_err(|m| format!("Tensor.from: {}", m))?;
        let rank = dims.len();
        let count: i64 = dims.iter().product();
        debug_assert_eq!(count as usize, leaves.len());

        let i64_t = self.context.i64_type();
        let mut leaf_vals: Vec<BasicValueEnum<'ctx>> = Vec::with_capacity(leaves.len());
        for leaf in &leaves {
            leaf_vals.push(self.compile_expr(leaf)?);
        }
        let elem = leaf_vals
            .first()
            .map(|v| v.get_type())
            .ok_or_else(|| "Tensor.from: cannot determine element type".to_string())?;
        let elem_size = self.tensor_elem_size(elem)?;

        let total = i64_t.const_int(8 * (1 + rank as u64) + (count as u64) * elem_size, false);
        let t_ptr = self
            .builder
            .build_call(self.malloc_fn, &[total.into()], "t.alloc")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        let rank_slot = self.tensor_header_slot(t_ptr, 0, "t.rank.p");
        self.builder
            .build_store(rank_slot, i64_t.const_int(rank as u64, false))
            .unwrap();
        for (i, d) in dims.iter().enumerate() {
            let slot = self.tensor_header_slot(t_ptr, 1 + i as u64, &format!("t.d{}.p", i));
            self.builder
                .build_store(slot, i64_t.const_int(*d as u64, false))
                .unwrap();
        }
        let data = self.tensor_data_ptr(t_ptr, rank, "t.data");
        for (i, v) in leaf_vals.into_iter().enumerate() {
            let slot = unsafe {
                self.builder
                    .build_gep(
                        elem,
                        data,
                        &[i64_t.const_int(i as u64, false)],
                        &format!("t.e{}.p", i),
                    )
                    .unwrap()
            };
            self.builder.build_store(slot, v).unwrap();
        }
        Ok(t_ptr.into())
    }

    // ── Indexing ────────────────────────────────────────────────

    /// `t[i, j, k]` read — the parser desugars to a single tuple index.
    /// Per-dim bounds checks (unsigned compare covers negatives);
    /// elided entirely when both the index component and the dim are
    /// compile-time constants (the typechecker already proved literal
    /// indices against static dims). Offset is the Horner fold
    /// `((i0)*d1 + i1)*d2 + i2 ...` with constant dims folded.
    pub(super) fn compile_tensor_index(
        &mut self,
        t_ptr: PointerValue<'ctx>,
        info: &TensorVarInfo<'ctx>,
        index: &Expr,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let elem = info.elem;
        let (data, slot) = self.tensor_index_elem_ptr(t_ptr, info, index)?;
        let _ = data;
        Ok(self.builder.build_load(elem, slot, "t.elem").unwrap())
    }

    /// `t[i, j, k] = v` store.
    pub(super) fn compile_tensor_index_store(
        &mut self,
        t_ptr: PointerValue<'ctx>,
        info: &TensorVarInfo<'ctx>,
        index: &Expr,
        val: BasicValueEnum<'ctx>,
    ) -> Result<(), String> {
        let (_, slot) = self.tensor_index_elem_ptr(t_ptr, info, index)?;
        self.builder.build_store(slot, val).unwrap();
        Ok(())
    }

    /// Shared get/set path: evaluate index components, bounds-check,
    /// fold the C-order offset, GEP the element slot.
    fn tensor_index_elem_ptr(
        &mut self,
        t_ptr: PointerValue<'ctx>,
        info: &TensorVarInfo<'ctx>,
        index: &Expr,
    ) -> Result<(PointerValue<'ctx>, PointerValue<'ctx>), String> {
        let rank = info.dims.len();
        let components: Vec<&Expr> = match &index.kind {
            ExprKind::Tuple(parts) if rank > 1 => parts.iter().collect(),
            _ => vec![index],
        };
        if components.len() != rank {
            return Err(format!(
                "tensor index has {} component(s), expected rank {}",
                components.len(),
                rank
            ));
        }
        let i64_t = self.context.i64_type();
        let mut offset: Option<IntValue<'ctx>> = None;
        for (i, comp) in components.iter().enumerate() {
            let idx_literal = match &comp.kind {
                ExprKind::Integer(v, _) => Some(*v),
                _ => None,
            };
            let idx_val = self.compile_expr(comp)?.into_int_value();
            // Bounds check — elided when both index and dim are
            // compile-time constants (typechecker-proven). Unsigned
            // `uge` rejects negatives in the same compare.
            let statically_proven = matches!((idx_literal, info.dims[i]), (Some(_), Some(_)));
            if !statically_proven {
                let dim_val = self.tensor_dim_value(t_ptr, info, i);
                let oob = self
                    .builder
                    .build_int_compare(IntPredicate::UGE, idx_val, dim_val, "t.idx.oob")
                    .unwrap();
                let ok = self.builder.build_not(oob, "t.idx.ok").unwrap();
                self.emit_tensor_guard(ok, &format!("tensor index out of bounds for dim {}", i))?;
            }
            offset = Some(match offset {
                None => idx_val,
                Some(acc) => {
                    let dim_val = self.tensor_dim_value(t_ptr, info, i);
                    let scaled = self.builder.build_int_mul(acc, dim_val, "t.off.s").unwrap();
                    self.builder
                        .build_int_add(scaled, idx_val, "t.off")
                        .unwrap()
                }
            });
        }
        let offset = offset.unwrap_or_else(|| i64_t.const_int(0, false));
        let data = self.tensor_data_ptr(t_ptr, rank, "t.data");
        let slot = unsafe {
            self.builder
                .build_gep(info.elem, data, &[offset], "t.elem.p")
                .unwrap()
        };
        Ok((data, slot))
    }

    // ── Instance methods ────────────────────────────────────────

    /// `t.shape()` -> Vec[i64] (fresh heap Vec copying the header dims)
    /// and `t.rank()` -> i64. Both read the runtime header, so they
    /// work uniformly for static, `?`-bearing, and splice-generic
    /// receivers.
    pub(super) fn compile_tensor_shape_method(
        &mut self,
        t_ptr: PointerValue<'ctx>,
        method: &str,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let i64_t = self.context.i64_type();
        let rank_val = self
            .builder
            .build_load(i64_t, t_ptr, "t.rank")
            .unwrap()
            .into_int_value();
        if method == "rank" {
            return Ok(rank_val.into());
        }
        // shape(): malloc rank*8, copy dims, return {ptr, rank, rank}.
        let bytes = self
            .builder
            .build_int_mul(rank_val, i64_t.const_int(8, false), "t.shape.bytes")
            .unwrap();
        let buf = self
            .builder
            .build_call(self.malloc_fn, &[bytes.into()], "t.shape.buf")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        let dims_base = self.tensor_header_slot(t_ptr, 1, "t.dims.base");
        let memcpy = self
            .builder
            .build_memcpy(buf, 8, dims_base, 8, bytes)
            .map_err(|e| format!("Tensor.shape memcpy failed: {:?}", e));
        memcpy?;
        let vec_ty = self.vec_struct_type();
        let mut agg = vec_ty.get_undef();
        agg = self
            .builder
            .build_insert_value(agg, buf, 0, "t.shape.data")
            .unwrap()
            .into_struct_value();
        agg = self
            .builder
            .build_insert_value(agg, rank_val, 1, "t.shape.len")
            .unwrap()
            .into_struct_value();
        agg = self
            .builder
            .build_insert_value(agg, rank_val, 2, "t.shape.cap")
            .unwrap()
            .into_struct_value();
        Ok(agg.into())
    }

    // ── Shared emission helpers ─────────────────────────────────

    /// Branch-to-panic guard: continue in a fresh block when `ok`,
    /// panic with `message` otherwise.
    fn emit_tensor_guard(&mut self, ok: IntValue<'ctx>, message: &str) -> Result<(), String> {
        let fn_val = self
            .current_fn
            .ok_or_else(|| "tensor guard outside function".to_string())?;
        let fail_bb = self.context.append_basic_block(fn_val, "t.guard.fail");
        let ok_bb = self.context.append_basic_block(fn_val, "t.guard.ok");
        self.builder
            .build_conditional_branch(ok, ok_bb, fail_bb)
            .unwrap();
        self.builder.position_at_end(fail_bb);
        self.emit_panic(message);
        self.builder.build_unreachable().unwrap();
        self.builder.position_at_end(ok_bb);
        Ok(())
    }

    /// Free `data` when `cap > 0` (temporary dims-Vec disposal).
    fn emit_free_if_cap_positive(&mut self, data: PointerValue<'ctx>, cap: IntValue<'ctx>) {
        let fn_val = self.current_fn.unwrap();
        let i64_t = self.context.i64_type();
        let do_bb = self.context.append_basic_block(fn_val, "t.tmpfree.do");
        let join_bb = self.context.append_basic_block(fn_val, "t.tmpfree.join");
        let pos = self
            .builder
            .build_int_compare(
                IntPredicate::SGT,
                cap,
                i64_t.const_int(0, false),
                "t.tmpfree.pos",
            )
            .unwrap();
        self.builder
            .build_conditional_branch(pos, do_bb, join_bb)
            .unwrap();
        self.builder.position_at_end(do_bb);
        self.builder
            .build_call(self.free_fn, &[data.into()], "")
            .unwrap();
        self.builder.build_unconditional_branch(join_bb).unwrap();
        self.builder.position_at_end(join_bb);
    }

    /// `for i in 0..count { data[i] = fill }`.
    fn emit_tensor_fill_loop(
        &mut self,
        data: PointerValue<'ctx>,
        elem: BasicTypeEnum<'ctx>,
        count: IntValue<'ctx>,
        fill: BasicValueEnum<'ctx>,
    ) {
        let fn_val = self.current_fn.unwrap();
        let i64_t = self.context.i64_type();
        let header_bb = self.context.append_basic_block(fn_val, "t.fill.head");
        let body_bb = self.context.append_basic_block(fn_val, "t.fill.body");
        let exit_bb = self.context.append_basic_block(fn_val, "t.fill.exit");
        let idx_slot = self.create_entry_alloca(fn_val, "t.fill.i", i64_t.into());
        self.builder
            .build_store(idx_slot, i64_t.const_int(0, false))
            .unwrap();
        self.builder.build_unconditional_branch(header_bb).unwrap();
        self.builder.position_at_end(header_bb);
        let i = self
            .builder
            .build_load(i64_t, idx_slot, "t.fill.iv")
            .unwrap()
            .into_int_value();
        let cont = self
            .builder
            .build_int_compare(IntPredicate::ULT, i, count, "t.fill.cont")
            .unwrap();
        self.builder
            .build_conditional_branch(cont, body_bb, exit_bb)
            .unwrap();
        self.builder.position_at_end(body_bb);
        let slot = unsafe {
            self.builder
                .build_gep(elem, data, &[i], "t.fill.p")
                .unwrap()
        };
        self.builder.build_store(slot, fill).unwrap();
        let next = self
            .builder
            .build_int_add(i, i64_t.const_int(1, false), "t.fill.next")
            .unwrap();
        self.builder.build_store(idx_slot, next).unwrap();
        self.builder.build_unconditional_branch(header_bb).unwrap();
        self.builder.position_at_end(exit_bb);
    }

    /// Element size in bytes for the supported element types.
    fn tensor_elem_size(&self, elem: BasicTypeEnum<'ctx>) -> Result<u64, String> {
        match elem {
            BasicTypeEnum::FloatType(ft) => {
                if ft == self.context.f64_type() {
                    Ok(8)
                } else {
                    Ok(4)
                }
            }
            BasicTypeEnum::IntType(it) => Ok((it.get_bit_width() as u64).div_ceil(8)),
            other => Err(format!(
                "Tensor element type {:?} is not yet supported in codegen — \
                 numeric primitives and bool only",
                other
            )),
        }
    }

    /// Register a tensor binding's cleanup (scope-exit free of the
    /// single heap block). Mirrors `track_vec_var`.
    pub(super) fn track_tensor_var(&mut self, tensor_alloca: PointerValue<'ctx>) {
        if let Some(frame) = self.scope_cleanup_actions.last_mut() {
            frame.push(super::state::CleanupAction::FreeTensor { tensor_alloca });
        }
    }

    /// Load the tensor pointer from a binding's slot.
    pub(super) fn tensor_ptr_for_var(&self, name: &str) -> Result<PointerValue<'ctx>, String> {
        let slot = self
            .variables
            .get(name)
            .ok_or_else(|| format!("Undefined tensor variable '{}'", name))?;
        Ok(self
            .builder
            .build_load(
                self.context.ptr_type(AddressSpace::default()),
                slot.ptr,
                &format!("{}.t", name),
            )
            .unwrap()
            .into_pointer_value())
    }
}

/// Syntax walk for `Tensor.from`'s literal argument — codegen twin of
/// the interpreter's `collect_tensor_literal_dims`. The typechecker
/// already rejected ragged/empty/mixed literals, so this only needs the
/// happy path plus defensive errors (codegen can be reached with
/// `KARAC_SKIP_TYPECHECK`-style bypasses in tests).
fn collect_tensor_literal<'e>(
    expr: &'e Expr,
    depth: usize,
    dims: &mut Vec<i64>,
    leaves: &mut Vec<&'e Expr>,
) -> Result<(), String> {
    let ExprKind::ArrayLiteral(elements) = &expr.kind else {
        return Err("argument must be a (nested) array literal".to_string());
    };
    if elements.is_empty() {
        return Err("empty literal level".to_string());
    }
    if dims.len() == depth {
        dims.push(elements.len() as i64);
    } else if dims[depth] != elements.len() as i64 {
        return Err("ragged literal".to_string());
    }
    let nested = matches!(elements[0].kind, ExprKind::ArrayLiteral(_));
    if nested {
        for e in elements {
            collect_tensor_literal(e, depth + 1, dims, leaves)?;
        }
    } else {
        leaves.extend(elements.iter());
    }
    Ok(())
}
