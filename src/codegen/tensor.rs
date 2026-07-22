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
use inkwell::values::{BasicValueEnum, FunctionValue, IntValue, PointerValue};
use inkwell::{AddressSpace, IntPredicate};

use crate::ast::{
    BinOp, CallArg, Expr, ExprKind, GenericArg, PatternKind, ShapeDim, TypeExpr, TypeKind, UnaryOp,
};
use crate::reduce_kernel::ReduceOp;
use crate::token::Span;

use super::kernel::{ContainerAccess, MapDest, MapKernelOp, MapOther, SortKey};
use super::state::{TensorVarInfo, VarSlot};

/// True iff `te` names an unsigned integer primitive — drives the
/// `is_unsigned` flag for per-element div/rem in the element-wise loop.
pub(super) fn type_expr_is_unsigned_int(te: &TypeExpr) -> bool {
    let TypeKind::Path(p) = &te.kind else {
        return false;
    };
    matches!(
        p.segments.last().map(String::as_str),
        Some("u8" | "u16" | "u32" | "u64" | "u128" | "usize")
    )
}

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
            elem_unsigned: type_expr_is_unsigned_int(&ti.elem),
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
        let mut elem_unsigned = false;
        let mut dims: Option<Vec<Option<i64>>> = None;
        for ga in gargs {
            match ga {
                GenericArg::Type(t) if elem.is_none() => {
                    elem = Some(self.llvm_type_for_type_expr(t));
                    elem_unsigned = type_expr_is_unsigned_int(t);
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
            elem_unsigned,
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
                        // dims temp is a `Vec[i64]` — element ABI size 8.
                        self.emit_free_if_cap_positive(data, cap, 8);
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
                    // Coerce the fill to the annotated element BEFORE the store
                    // loop — a bare `2.5` compiles to f64 and a bare `7` to i64,
                    // so filling a narrow / f32 tensor would write 8 bytes into
                    // an `elem`-strided slot (B-2026-07-03-35 class: `full([2],
                    // 2.5)` over an f32 tensor read back 0.0). `ones` already
                    // builds an `elem`-typed constant, so it needs no coercion.
                    let raw = self.compile_expr(&val_arg.value)?;
                    self.coerce_scalar_to_tensor_elem(raw, info.elem)
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
        let leaf_elem = leaf_vals
            .first()
            .map(|v| v.get_type())
            .ok_or_else(|| "Tensor.from: cannot determine element type".to_string())?;
        // Prefer the DECLARED element type from the binding annotation (threaded
        // via `pending_let_tensor_info`, the same mechanism zeros/ones/full use).
        // The leaf literals compile at the default width (i64 for ints, f64 for
        // floats) because there is no expected-type context at their compile
        // site, so for a NARROW annotation (`Tensor[i32,…]` / `[f32,…]` / `[u8,…]`)
        // the stored width would be 8 bytes while every reader (`t[i]`, `t.sum()`,
        // map/fold/sorted, …) strides at `tensor_elem_size(info.elem)` — an
        // 8-byte-write / narrow-read mismatch that silently corrupts element reads
        // (B-2026-07-03-35). Store at the declared width and coerce each leaf to
        // it. The rank guard keeps a stale pending (belonging to an unrelated
        // outer tensor binding) from hijacking the element type; a bare/
        // unannotated `Tensor.from` (no pending) falls back to the leaf width,
        // which the reader-side var info infers identically.
        let elem = match &self.pending_let_tensor_info {
            Some(info) if info.dims.len() == rank => info.elem,
            _ => leaf_elem,
        };
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
            // Narrow (or widen) the leaf to the declared element width so the
            // store stride matches every reader's `tensor_elem_size(elem)`.
            // `coerce_scalar_to_tensor_elem` additionally handles int→float
            // (`sitofp`) for an integer literal in a float tensor (`Tensor[f64]
            // = Tensor.from([1, 2, 3])`), which `coerce_scalar_to_type` omits.
            let v = self.coerce_scalar_to_tensor_elem(v, elem);
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
        // Coerce the rhs to the element's LLVM type before storing
        // (B-2026-07-22-7). A bare `5.0` literal compiles to `f64`; storing
        // it into an `f32` element slot without an `fptrunc` writes 8 bytes
        // where 4 are expected, and a later `f32` read gets the low half —
        // which is zero for many round values (`double 5.0` low word = 0),
        // so `t[0] = 5.0; t[0]` silently read 0. An int literal (`i64`
        // default) into a float element is the same class (the low word of
        // `i64 5` reads as a tiny f32 denormal). The scalar `let x: f32 =
        // 5.0` path already coerces; the tensor index-store just wasn't
        // routing through it. `sitofp` first for an int rhs into a float
        // element, then `coerce_scalar_to_type` for float↔float / int↔int
        // width.
        let val = match (val, info.elem) {
            (BasicValueEnum::IntValue(iv), BasicTypeEnum::FloatType(ft)) => self
                .builder
                .build_signed_int_to_float(iv, ft, "t.store.i2f")
                .unwrap()
                .into(),
            _ => self.coerce_scalar_to_type(val, info.elem),
        };
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
            let idx_raw = self.compile_expr(comp)?;
            let idx_val = self.coerce_to_i64(idx_raw)?;
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

    // ── Shape-transform family ──────────────────────────────────
    //
    // `reshape` / `permute` / `slice` / `squeeze` each produce a FRESH
    // tensor (a copy — tensors are value types). The receiver is
    // borrowed, never consumed: a `let r = t.reshape(...)` RHS is a
    // `MethodCall`, not a bare identifier, so the let-binding's
    // `suppress_source_vec_cleanup_for_arg` no-ops and the receiver
    // keeps its own `FreeTensor`; the result is registered + tracked by
    // the existing let-binding machinery from the lowering side-table at
    // the call span (the call's result type survives the
    // MethodCall-shares-receiver-span collision because it is the last
    // write at that key). Static receiver dims are never needed here:
    // rank and dims are read from the runtime header, which is always
    // authoritative; only the element type comes from static info (the
    // result side-table entry — element type is invariant across all
    // four transforms). Every compile-time check the typechecker makes
    // is re-emitted as a runtime guard, because `karac run`'s
    // `run_program` path doesn't gate on typecheck errors and a bypassed
    // typecheck must trap rather than corrupt memory. The interpreter
    // twins live in `src/interpreter/method_call_tensor.rs`.

    /// Dispatch the shape-transform family. Returns `Ok(None)` when
    /// `method` isn't one of the four transforms or the receiver isn't a
    /// statically-ranked tensor (caller falls through). Handles both an
    /// identifier receiver (`t.reshape(...)`, pointer from the binding
    /// slot) and a chained / value receiver (`t.permute(..).reshape(..)`
    /// or `make().reshape(..)`, pointer from compiling the object). The
    /// chained gate is: the call's *result* is tensor-typed (recorded in
    /// the lowering side-table at the call span) — these four method
    /// names only yield a tensor when the receiver is a tensor, so a
    /// side-table hit proves a tensor receiver without re-deriving its
    /// type.
    pub(super) fn try_compile_tensor_transform(
        &mut self,
        object: &Expr,
        method: &str,
        args: &[CallArg],
        call_span: &Span,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        if !matches!(
            method,
            "reshape" | "permute" | "slice" | "squeeze" | "transpose" | "matmul"
        ) {
            return Ok(None);
        }
        // A receiver that is a fresh OWNED tensor temporary — a chained
        // transform (`a.permute(..).reshape(..)`), a free-fn return
        // (`make().reshape(..)`), or a non-transform method return that
        // hands back an owned tensor — was malloc'd upstream, is copied
        // out of by this call, and is owned by nothing else; free it
        // after the copy so the intermediate doesn't leak. An identifier
        // / field / index receiver is borrowed and must NOT be freed
        // here (it keeps its own scope cleanup), and a *borrowed*
        // (`ref Tensor`) return must not be freed either — freeing a
        // borrow would corrupt the owner. `tensor_receiver_is_owned_fresh_temp`
        // draws that line via the ref-return side-tables.
        let receiver_is_fresh_temp = self.tensor_receiver_is_owned_fresh_temp(object);
        let t_ptr = match &object.kind {
            ExprKind::Identifier(name) if self.tensor_var_infos.contains_key(name.as_str()) => {
                self.tensor_ptr_for_var(name)?
            }
            _ if self
                .tensor_typed_exprs
                .contains_key(&(call_span.offset, call_span.length)) =>
            {
                self.compile_expr(object)?.into_pointer_value()
            }
            _ => return Ok(None),
        };
        // matmul's right-hand tensor: resolve its pointer the same two ways
        // as the receiver (identifier binding slot / compiled value expr),
        // and free it after the copy when it is a fresh owned temp — the
        // same ownership line the receiver draws.
        let mut matmul_arg: Option<(PointerValue<'ctx>, bool)> = None;
        if method == "matmul" {
            let arg_expr = args
                .first()
                .map(|a| &a.value)
                .ok_or_else(|| "matmul takes exactly 1 argument".to_string())?;
            let arg_is_fresh_temp = self.tensor_receiver_is_owned_fresh_temp(arg_expr);
            let o_ptr = match &arg_expr.kind {
                ExprKind::Identifier(name) if self.tensor_var_infos.contains_key(name.as_str()) => {
                    self.tensor_ptr_for_var(name)?
                }
                _ => self.compile_expr(arg_expr)?.into_pointer_value(),
            };
            matmul_arg = Some((o_ptr, arg_is_fresh_temp));
        }
        let v = match method {
            "reshape" => self.compile_tensor_reshape(t_ptr, args, call_span)?,
            "permute" => self.compile_tensor_permute(t_ptr, args, call_span)?,
            "slice" => self.compile_tensor_slice(t_ptr, args, call_span)?,
            "squeeze" => self.compile_tensor_squeeze(t_ptr, args, call_span)?,
            "transpose" => self.compile_tensor_transpose(t_ptr, args, call_span)?,
            "matmul" => {
                let (o_ptr, _) = matmul_arg.expect("matmul arg resolved above");
                self.compile_tensor_matmul(t_ptr, o_ptr, call_span)?
            }
            _ => unreachable!(),
        };
        if receiver_is_fresh_temp {
            self.builder
                .build_call(self.free_fn, &[t_ptr.into()], "")
                .unwrap();
        }
        if let Some((o_ptr, true)) = matmul_arg {
            self.builder
                .build_call(self.free_fn, &[o_ptr.into()], "")
                .unwrap();
        }
        Ok(Some(v))
    }

    /// Is `object` — the receiver of a shape method — a *fresh owned*
    /// tensor temporary that this call must free after copying out of it?
    ///
    /// True for the three sources of a malloc'd-here-and-owned-nowhere-else
    /// tensor: a chained shape-transform call, a free-function call, and a
    /// non-transform method call. The hazard a naive "any Call/MethodCall"
    /// rule would hit is a *borrowed* return — `fn first(t: ref Tensor[..])
    /// -> ref Tensor[..]` hands back a pointer into a tensor the caller
    /// still owns, and freeing that would corrupt the owner. The compiler
    /// records every `ref`/`mut ref` return so the *absence* of that
    /// record is the owned-return signal: free-fn calls consult
    /// `fn_ref_return_inner` (keyed by callee name), method calls consult
    /// `ref_return_inner_types` (keyed by the call span — `MethodCall.span
    /// == receiver.span`, which is `object.span` here). An identifier /
    /// field / index receiver is borrowed (a live binding owns it) and is
    /// never a fresh temp — those keep their own scope cleanup.
    fn tensor_receiver_is_owned_fresh_temp(&self, object: &Expr) -> bool {
        match &object.kind {
            // A chained transform always returns a freshly malloc'd, owned
            // block (the transforms never return a borrow), so it is owned
            // regardless of any side-table entry.
            ExprKind::MethodCall { method: m, .. }
                if matches!(
                    m.as_str(),
                    "reshape" | "permute" | "slice" | "squeeze" | "transpose" | "matmul"
                ) =>
            {
                true
            }
            // Free-function return: owned unless the callee's declared
            // return type is a borrow (`ref`/`mut ref`). The declared type
            // is the durable signal — `fn_ref_return_inner` is NOT set for
            // a `ref Tensor` return (tensors use the by-value ref ABI, see
            // `ref_return_is_value_abi`), so a borrowed tensor return would
            // wrongly read as owned if we keyed off that table. A
            // non-identifier callee (a qualified constructor like
            // `Tensor.zeros(..)`) is left alone — a benign leak, not worth a
            // free we can't prove safe.
            ExprKind::Call { callee, .. } => match &callee.kind {
                ExprKind::Identifier(n) => !matches!(
                    self.fn_return_type_exprs.get(n.as_str()).map(|t| &t.kind),
                    Some(TypeKind::Ref(_) | TypeKind::MutRef(_))
                ),
                _ => false,
            },
            // Non-transform method return: owned unless it is a user
            // accessor that returns a borrow (`-> ref T`, recorded by NAME
            // in `user_ref_method_names`). The method NAME is the
            // span-collision-immune signal: `MethodCall.span ==
            // receiver.span`, so a chained `h.view().permute(..)` records
            // the *outer* (owned `Tensor`) type at that shared span, which
            // would make a span-keyed `ref_return_inner_types` lookup
            // wrongly read the `h.view()` borrow as owned and free the
            // owner's block (a use-after-free for any later `h.view()`).
            ExprKind::MethodCall { method, .. } => !self.user_ref_method_names.contains(method),
            _ => false,
        }
    }

    /// Element LLVM type of a transform's *result* tensor, read from the
    /// lowering side-table keyed by the call span. The result of every
    /// transform here is itself a `Tensor[T, …]`, so the entry exists;
    /// element type is invariant across the transforms, so this is also
    /// the receiver's element type (used for element GEPs and the byte
    /// size of the data copy).
    fn tensor_transform_elem(&self, call_span: &Span) -> Result<BasicTypeEnum<'ctx>, String> {
        let key = (call_span.offset, call_span.length);
        let ti = self.tensor_typed_exprs.get(&key).ok_or_else(|| {
            "tensor shape-transform result type is not statically known \
             (missing lowering side-table entry)"
                .to_string()
        })?;
        Ok(self.llvm_type_for_type_expr(&ti.elem.clone()))
    }

    /// Load the rank from header slot 0.
    fn tensor_load_rank(&self, t_ptr: PointerValue<'ctx>) -> IntValue<'ctx> {
        self.builder
            .build_load(self.context.i64_type(), t_ptr, "t.rank")
            .unwrap()
            .into_int_value()
    }

    /// Pointer to dim slot `i` for a runtime `i`: `gep i64, t_ptr,
    /// [1 + i]` (slot 0 is rank, dims start at slot 1).
    fn tensor_dim_slot_dyn(
        &self,
        t_ptr: PointerValue<'ctx>,
        i_val: IntValue<'ctx>,
        name: &str,
    ) -> PointerValue<'ctx> {
        let i64_t = self.context.i64_type();
        let off = self
            .builder
            .build_int_add(i_val, i64_t.const_int(1, false), "t.dimoff")
            .unwrap();
        unsafe { self.builder.build_gep(i64_t, t_ptr, &[off], name).unwrap() }
    }

    /// Data pointer for a runtime rank: `gep i64, t_ptr, [1 + rank]`.
    fn tensor_data_ptr_dyn(
        &self,
        t_ptr: PointerValue<'ctx>,
        rank_val: IntValue<'ctx>,
        name: &str,
    ) -> PointerValue<'ctx> {
        let i64_t = self.context.i64_type();
        let off = self
            .builder
            .build_int_add(rank_val, i64_t.const_int(1, false), "t.dataoff")
            .unwrap();
        unsafe { self.builder.build_gep(i64_t, t_ptr, &[off], name).unwrap() }
    }

    /// `acc = 1; for i in 0..rank { acc *= dim[i] }` — element count read
    /// from the header (works for any runtime rank / dim mix).
    fn tensor_count_runtime(
        &mut self,
        t_ptr: PointerValue<'ctx>,
        rank_val: IntValue<'ctx>,
    ) -> IntValue<'ctx> {
        let fn_val = self.current_fn.unwrap();
        let i64_t = self.context.i64_type();
        let acc = self.create_entry_alloca(fn_val, "t.cnt", i64_t.into());
        self.builder
            .build_store(acc, i64_t.const_int(1, false))
            .unwrap();
        let iv = self.create_entry_alloca(fn_val, "t.cnt.i", i64_t.into());
        self.builder
            .build_store(iv, i64_t.const_int(0, false))
            .unwrap();
        let head = self.context.append_basic_block(fn_val, "t.cnt.head");
        let body = self.context.append_basic_block(fn_val, "t.cnt.body");
        let exit = self.context.append_basic_block(fn_val, "t.cnt.exit");
        self.builder.build_unconditional_branch(head).unwrap();
        self.builder.position_at_end(head);
        let i = self
            .builder
            .build_load(i64_t, iv, "t.cnt.iv")
            .unwrap()
            .into_int_value();
        let cont = self
            .builder
            .build_int_compare(IntPredicate::ULT, i, rank_val, "t.cnt.cont")
            .unwrap();
        self.builder
            .build_conditional_branch(cont, body, exit)
            .unwrap();
        self.builder.position_at_end(body);
        let slot = self.tensor_dim_slot_dyn(t_ptr, i, "t.cnt.dp");
        let d = self
            .builder
            .build_load(i64_t, slot, "t.cnt.d")
            .unwrap()
            .into_int_value();
        let a = self
            .builder
            .build_load(i64_t, acc, "t.cnt.a")
            .unwrap()
            .into_int_value();
        let m = self.builder.build_int_mul(a, d, "t.cnt.m").unwrap();
        self.builder.build_store(acc, m).unwrap();
        let ni = self
            .builder
            .build_int_add(i, i64_t.const_int(1, false), "t.cnt.ni")
            .unwrap();
        self.builder.build_store(iv, ni).unwrap();
        self.builder.build_unconditional_branch(head).unwrap();
        self.builder.position_at_end(exit);
        self.builder
            .build_load(i64_t, acc, "t.cnt.v")
            .unwrap()
            .into_int_value()
    }

    /// Allocate a result tensor block for a runtime `rank_val` and
    /// `count`, write the rank into slot 0, and return `(t_ptr,
    /// data_ptr)`. The caller writes the per-dim header slots and the
    /// data. `bytes = 8*(1+rank) + count*elem_size`.
    fn tensor_alloc_runtime(
        &mut self,
        rank_val: IntValue<'ctx>,
        count: IntValue<'ctx>,
        elem_size: u64,
    ) -> (PointerValue<'ctx>, PointerValue<'ctx>) {
        let i64_t = self.context.i64_type();
        let rank_p1 = self
            .builder
            .build_int_add(rank_val, i64_t.const_int(1, false), "t.rankp1")
            .unwrap();
        let header_bytes = self
            .builder
            .build_int_mul(rank_p1, i64_t.const_int(8, false), "t.hbytes")
            .unwrap();
        let data_bytes = self
            .builder
            .build_int_mul(count, i64_t.const_int(elem_size, false), "t.dbytes")
            .unwrap();
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
        self.builder.build_store(t_ptr, rank_val).unwrap();
        let data = self.tensor_data_ptr_dyn(t_ptr, rank_val, "t.res.data");
        (t_ptr, data)
    }

    /// `t.reshape([d0, d1, ...])` — same elements, new dims, C-order
    /// preserved (a copy). The dims argument is an array literal (the
    /// typechecker's rule; the result's static rank is its length).
    /// Integer-literal entries are folded; runtime entries get a
    /// non-negative guard. The element-count product is asserted equal to
    /// the receiver's at runtime.
    pub(super) fn compile_tensor_reshape(
        &mut self,
        t_ptr: PointerValue<'ctx>,
        args: &[CallArg],
        call_span: &Span,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let elem = self.tensor_transform_elem(call_span)?;
        let elem_size = self.tensor_elem_size(elem)?;
        let i64_t = self.context.i64_type();
        let entries = match args.first().map(|a| &a.value.kind) {
            Some(ExprKind::ArrayLiteral(e)) if !e.is_empty() => e.clone(),
            _ => return Err("reshape requires a non-empty array-literal dims argument".to_string()),
        };
        let result_rank = entries.len();
        let mut new_dims: Vec<IntValue<'ctx>> = Vec::with_capacity(result_rank);
        for entry in &entries {
            let is_literal = matches!(entry.kind, ExprKind::Integer(_, _));
            let dv = self.compile_expr(entry)?.into_int_value();
            if !is_literal {
                let neg = self
                    .builder
                    .build_int_compare(IntPredicate::SLT, dv, i64_t.const_zero(), "t.rsh.neg")
                    .unwrap();
                let ok = self.builder.build_not(neg, "t.rsh.nn").unwrap();
                self.emit_tensor_guard(ok, "reshape dim must be non-negative")?;
            }
            new_dims.push(dv);
        }
        let mut new_count = i64_t.const_int(1, false);
        for dv in &new_dims {
            new_count = self
                .builder
                .build_int_mul(new_count, *dv, "t.rsh.cnt")
                .unwrap();
        }
        let rank_val = self.tensor_load_rank(t_ptr);
        let old_count = self.tensor_count_runtime(t_ptr, rank_val);
        let eq = self
            .builder
            .build_int_compare(IntPredicate::EQ, old_count, new_count, "t.rsh.eq")
            .unwrap();
        self.emit_tensor_guard(eq, "reshape element counts must match")?;
        let src_data = self.tensor_data_ptr_dyn(t_ptr, rank_val, "t.rsh.src");
        let result_rank_val = i64_t.const_int(result_rank as u64, false);
        let (res, res_data) = self.tensor_alloc_runtime(result_rank_val, new_count, elem_size);
        for (k, dv) in new_dims.iter().enumerate() {
            let slot = self.tensor_header_slot(res, 1 + k as u64, &format!("t.rsh.d{}.p", k));
            self.builder.build_store(slot, *dv).unwrap();
        }
        let bytes = self
            .builder
            .build_int_mul(new_count, i64_t.const_int(elem_size, false), "t.rsh.bytes")
            .unwrap();
        self.builder
            .build_memcpy(res_data, 8, src_data, 8, bytes)
            .map_err(|e| format!("reshape data copy failed: {:?}", e))?;
        Ok(res.into())
    }

    /// `t.permute([1, 0, 2])` — reorder the axes; result dim `i` is the
    /// receiver's dim `perm[i]`. `perm` is an array literal of integer
    /// literals forming an exact permutation of `0..rank` (typechecker
    /// rule). Data is reordered into a fresh C-order buffer: each output
    /// flat index decomposes into output coords, and the source flat
    /// index is the dot product of those coords with the *source*
    /// strides of the permuted-from axes.
    pub(super) fn compile_tensor_permute(
        &mut self,
        t_ptr: PointerValue<'ctx>,
        args: &[CallArg],
        call_span: &Span,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let entries = match args.first().map(|a| &a.value.kind) {
            Some(ExprKind::ArrayLiteral(e)) if !e.is_empty() => e.clone(),
            _ => return Err("permute requires a non-empty literal axis-list".to_string()),
        };
        let rank = entries.len();
        let mut perm: Vec<usize> = Vec::with_capacity(rank);
        for entry in &entries {
            match &entry.kind {
                ExprKind::Integer(v, _) if *v >= 0 && (*v as usize) < rank => {
                    perm.push(*v as usize)
                }
                _ => {
                    return Err(
                        "permute requires integer-literal axes forming a permutation of \
                                 0..rank"
                            .to_string(),
                    )
                }
            }
        }
        self.compile_tensor_permute_with_perm(t_ptr, &perm, call_span)
    }

    /// `t.transpose()` — reverse the axes: `permute([rank-1, …, 0])` with
    /// no axis list to write. The static rank comes from the lowering
    /// side-table's RESULT entry at the call span (transpose preserves
    /// rank, so result rank == receiver rank). B-2026-07-14-18 (was a
    /// phantom method: typechecker accepted it, no backend implemented it).
    pub(super) fn compile_tensor_transpose(
        &mut self,
        t_ptr: PointerValue<'ctx>,
        args: &[CallArg],
        call_span: &Span,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        if !args.is_empty() {
            return Err(format!(
                "transpose takes no arguments, found {}",
                args.len()
            ));
        }
        let key = (call_span.offset, call_span.length);
        let rank = self
            .tensor_typed_exprs
            .get(&key)
            .map(|ti| ti.dims.len())
            .ok_or_else(|| {
                "transpose result type is not statically known \
                 (missing lowering side-table entry)"
                    .to_string()
            })?;
        let perm: Vec<usize> = (0..rank).rev().collect();
        self.compile_tensor_permute_with_perm(t_ptr, &perm, call_span)
    }

    /// Shared core of `permute`/`transpose`: reorder the axes by `perm`
    /// (result dim `i` is the receiver's dim `perm[i]`), copying into a
    /// fresh result block. Rank is static (= `perm.len()`), so the
    /// coord-decompose loop unrolls.
    fn compile_tensor_permute_with_perm(
        &mut self,
        t_ptr: PointerValue<'ctx>,
        perm: &[usize],
        call_span: &Span,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let elem = self.tensor_transform_elem(call_span)?;
        let elem_size = self.tensor_elem_size(elem)?;
        let i64_t = self.context.i64_type();
        let fn_val = self.current_fn.unwrap();
        let rank = perm.len();
        // Receiver dims (rank is static = perm length).
        let rdims: Vec<IntValue<'ctx>> =
            (0..rank).map(|i| self.tensor_load_dim(t_ptr, i)).collect();
        // Source C-order strides: stride[rank-1] = 1, stride[k] =
        // stride[k+1] * rdims[k+1].
        let mut strides = vec![i64_t.const_int(1, false); rank];
        for k in (0..rank.saturating_sub(1)).rev() {
            strides[k] = self
                .builder
                .build_int_mul(strides[k + 1], rdims[k + 1], "t.prm.st")
                .unwrap();
        }
        let new_dims: Vec<IntValue<'ctx>> = perm.iter().map(|&p| rdims[p]).collect();
        let mut total = i64_t.const_int(1, false);
        for d in &rdims {
            total = self.builder.build_int_mul(total, *d, "t.prm.tot").unwrap();
        }
        let rank_val = i64_t.const_int(rank as u64, false);
        let src_data = self.tensor_data_ptr_dyn(t_ptr, rank_val, "t.prm.src");
        let (res, res_data) = self.tensor_alloc_runtime(rank_val, total, elem_size);
        for (i, dv) in new_dims.iter().enumerate() {
            let slot = self.tensor_header_slot(res, 1 + i as u64, &format!("t.prm.d{}.p", i));
            self.builder.build_store(slot, *dv).unwrap();
        }
        // Reorder loop: for f in 0..total.
        let fv = self.create_entry_alloca(fn_val, "t.prm.f", i64_t.into());
        self.builder
            .build_store(fv, i64_t.const_int(0, false))
            .unwrap();
        let head = self.context.append_basic_block(fn_val, "t.prm.head");
        let body = self.context.append_basic_block(fn_val, "t.prm.body");
        let exit = self.context.append_basic_block(fn_val, "t.prm.exit");
        self.builder.build_unconditional_branch(head).unwrap();
        self.builder.position_at_end(head);
        let f = self
            .builder
            .build_load(i64_t, fv, "t.prm.fv")
            .unwrap()
            .into_int_value();
        let cont = self
            .builder
            .build_int_compare(IntPredicate::ULT, f, total, "t.prm.cont")
            .unwrap();
        self.builder
            .build_conditional_branch(cont, body, exit)
            .unwrap();
        self.builder.position_at_end(body);
        // Decompose f into output coords (C-order over new_dims),
        // accumulating the source flat index. Output coord i indexes
        // source axis perm[i]; rank is static so this unrolls.
        let mut rem = f;
        let mut src = i64_t.const_int(0, false);
        for i in (0..rank).rev() {
            let nd = new_dims[i];
            let coord = self
                .builder
                .build_int_unsigned_rem(rem, nd, "t.prm.coord")
                .unwrap();
            rem = self
                .builder
                .build_int_unsigned_div(rem, nd, "t.prm.rem")
                .unwrap();
            let contrib = self
                .builder
                .build_int_mul(coord, strides[perm[i]], "t.prm.contrib")
                .unwrap();
            src = self
                .builder
                .build_int_add(src, contrib, "t.prm.srcacc")
                .unwrap();
        }
        let src_slot = unsafe {
            self.builder
                .build_gep(elem, src_data, &[src], "t.prm.srcp")
                .unwrap()
        };
        let v = self.builder.build_load(elem, src_slot, "t.prm.v").unwrap();
        let dst_slot = unsafe {
            self.builder
                .build_gep(elem, res_data, &[f], "t.prm.dstp")
                .unwrap()
        };
        self.builder.build_store(dst_slot, v).unwrap();
        let nf = self
            .builder
            .build_int_add(f, i64_t.const_int(1, false), "t.prm.nf")
            .unwrap();
        self.builder.build_store(fv, nf).unwrap();
        self.builder.build_unconditional_branch(head).unwrap();
        self.builder.position_at_end(exit);
        Ok(res.into())
    }

    /// `a.matmul(b)` — rank-2 matrix multiplication: `[m, k] × [k, n] →
    /// [m, n]`, C-order data, standard triple loop over runtime dims. The
    /// typechecker enforces rank-2 × rank-2 with matching numeric element
    /// types and statically-checkable inner dims; per module policy every
    /// compile-time check is re-emitted as a runtime guard (rank == 2 on
    /// both operands, inner dims equal). Element arithmetic follows the
    /// element LLVM type (float vs int). B-2026-07-14-18 (was a phantom
    /// method: typechecker accepted it, no backend implemented it).
    pub(super) fn compile_tensor_matmul(
        &mut self,
        t_ptr: PointerValue<'ctx>,
        o_ptr: PointerValue<'ctx>,
        call_span: &Span,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let elem = self.tensor_transform_elem(call_span)?;
        let elem_size = self.tensor_elem_size(elem)?;
        let i64_t = self.context.i64_type();
        let fn_val = self.current_fn.unwrap();
        let two = i64_t.const_int(2, false);

        // Runtime guards: both operands rank-2, inner dims equal.
        let a_rank = self.tensor_load_rank(t_ptr);
        let a_rank_ok = self
            .builder
            .build_int_compare(IntPredicate::EQ, a_rank, two, "t.mm.arank.ok")
            .unwrap();
        self.emit_tensor_guard(a_rank_ok, "matmul requires a rank-2 receiver")?;
        let b_rank = self.tensor_load_rank(o_ptr);
        let b_rank_ok = self
            .builder
            .build_int_compare(IntPredicate::EQ, b_rank, two, "t.mm.brank.ok")
            .unwrap();
        self.emit_tensor_guard(b_rank_ok, "matmul requires a rank-2 argument")?;
        let m = self.tensor_load_dim(t_ptr, 0);
        let k = self.tensor_load_dim(t_ptr, 1);
        let k2 = self.tensor_load_dim(o_ptr, 0);
        let n = self.tensor_load_dim(o_ptr, 1);
        let inner_ok = self
            .builder
            .build_int_compare(IntPredicate::EQ, k, k2, "t.mm.inner.ok")
            .unwrap();
        self.emit_tensor_guard(inner_ok, "matmul inner dimensions mismatch")?;

        // Result: rank-2 [m, n].
        let count = self.builder.build_int_mul(m, n, "t.mm.cnt").unwrap();
        let (res, res_data) = self.tensor_alloc_runtime(two, count, elem_size);
        let m_slot = self.tensor_header_slot(res, 1, "t.mm.d0.p");
        self.builder.build_store(m_slot, m).unwrap();
        let n_slot = self.tensor_header_slot(res, 2, "t.mm.d1.p");
        self.builder.build_store(n_slot, n).unwrap();
        let a_data = self.tensor_data_ptr_dyn(t_ptr, two, "t.mm.adata");
        let b_data = self.tensor_data_ptr_dyn(o_ptr, two, "t.mm.bdata");

        // Triple loop: for i in 0..m { for j in 0..n { acc = Σp a[i,p]*b[p,j] } }.
        let is_float = elem.is_float_type();
        let iv = self.create_entry_alloca(fn_val, "t.mm.i", i64_t.into());
        let jv = self.create_entry_alloca(fn_val, "t.mm.j", i64_t.into());
        let pv = self.create_entry_alloca(fn_val, "t.mm.p", i64_t.into());
        let accv = self.create_entry_alloca(fn_val, "t.mm.acc", elem);
        let zero = i64_t.const_int(0, false);
        let one = i64_t.const_int(1, false);
        let elem_zero: BasicValueEnum<'ctx> = if is_float {
            elem.into_float_type().const_zero().into()
        } else {
            elem.into_int_type().const_zero().into()
        };

        let i_head = self.context.append_basic_block(fn_val, "t.mm.i.head");
        let i_body = self.context.append_basic_block(fn_val, "t.mm.i.body");
        let i_exit = self.context.append_basic_block(fn_val, "t.mm.i.exit");
        let j_head = self.context.append_basic_block(fn_val, "t.mm.j.head");
        let j_body = self.context.append_basic_block(fn_val, "t.mm.j.body");
        let j_exit = self.context.append_basic_block(fn_val, "t.mm.j.exit");
        let p_head = self.context.append_basic_block(fn_val, "t.mm.p.head");
        let p_body = self.context.append_basic_block(fn_val, "t.mm.p.body");
        let p_exit = self.context.append_basic_block(fn_val, "t.mm.p.exit");

        self.builder.build_store(iv, zero).unwrap();
        self.builder.build_unconditional_branch(i_head).unwrap();
        self.builder.position_at_end(i_head);
        let i = self
            .builder
            .build_load(i64_t, iv, "t.mm.iv")
            .unwrap()
            .into_int_value();
        let i_cont = self
            .builder
            .build_int_compare(IntPredicate::SLT, i, m, "t.mm.i.cont")
            .unwrap();
        self.builder
            .build_conditional_branch(i_cont, i_body, i_exit)
            .unwrap();

        self.builder.position_at_end(i_body);
        self.builder.build_store(jv, zero).unwrap();
        self.builder.build_unconditional_branch(j_head).unwrap();
        self.builder.position_at_end(j_head);
        let j = self
            .builder
            .build_load(i64_t, jv, "t.mm.jv")
            .unwrap()
            .into_int_value();
        let j_cont = self
            .builder
            .build_int_compare(IntPredicate::SLT, j, n, "t.mm.j.cont")
            .unwrap();
        self.builder
            .build_conditional_branch(j_cont, j_body, j_exit)
            .unwrap();

        self.builder.position_at_end(j_body);
        self.builder.build_store(accv, elem_zero).unwrap();
        self.builder.build_store(pv, zero).unwrap();
        self.builder.build_unconditional_branch(p_head).unwrap();
        self.builder.position_at_end(p_head);
        let p = self
            .builder
            .build_load(i64_t, pv, "t.mm.pv")
            .unwrap()
            .into_int_value();
        let p_cont = self
            .builder
            .build_int_compare(IntPredicate::SLT, p, k, "t.mm.p.cont")
            .unwrap();
        self.builder
            .build_conditional_branch(p_cont, p_body, p_exit)
            .unwrap();

        self.builder.position_at_end(p_body);
        // a[i*k + p], b[p*n + j]
        let ik = self.builder.build_int_mul(i, k, "t.mm.ik").unwrap();
        let a_idx = self.builder.build_int_add(ik, p, "t.mm.aidx").unwrap();
        let pn = self.builder.build_int_mul(p, n, "t.mm.pn").unwrap();
        let b_idx = self.builder.build_int_add(pn, j, "t.mm.bidx").unwrap();
        let a_slot = unsafe {
            self.builder
                .build_gep(elem, a_data, &[a_idx], "t.mm.ap")
                .unwrap()
        };
        let b_slot = unsafe {
            self.builder
                .build_gep(elem, b_data, &[b_idx], "t.mm.bp")
                .unwrap()
        };
        let av = self.builder.build_load(elem, a_slot, "t.mm.av").unwrap();
        let bv = self.builder.build_load(elem, b_slot, "t.mm.bv").unwrap();
        let acc = self.builder.build_load(elem, accv, "t.mm.accv").unwrap();
        let new_acc: BasicValueEnum<'ctx> = if is_float {
            let prod = self
                .builder
                .build_float_mul(av.into_float_value(), bv.into_float_value(), "t.mm.prod")
                .unwrap();
            self.builder
                .build_float_add(acc.into_float_value(), prod, "t.mm.nacc")
                .unwrap()
                .into()
        } else {
            let prod = self
                .builder
                .build_int_mul(av.into_int_value(), bv.into_int_value(), "t.mm.prod")
                .unwrap();
            self.builder
                .build_int_add(acc.into_int_value(), prod, "t.mm.nacc")
                .unwrap()
                .into()
        };
        self.builder.build_store(accv, new_acc).unwrap();
        let np = self.builder.build_int_add(p, one, "t.mm.np").unwrap();
        self.builder.build_store(pv, np).unwrap();
        self.builder.build_unconditional_branch(p_head).unwrap();

        self.builder.position_at_end(p_exit);
        // out[i*n + j] = acc
        let in_ = self.builder.build_int_mul(i, n, "t.mm.in").unwrap();
        let out_idx = self.builder.build_int_add(in_, j, "t.mm.oidx").unwrap();
        let out_slot = unsafe {
            self.builder
                .build_gep(elem, res_data, &[out_idx], "t.mm.op")
                .unwrap()
        };
        let final_acc = self.builder.build_load(elem, accv, "t.mm.facc").unwrap();
        self.builder.build_store(out_slot, final_acc).unwrap();
        let nj = self.builder.build_int_add(j, one, "t.mm.nj").unwrap();
        self.builder.build_store(jv, nj).unwrap();
        self.builder.build_unconditional_branch(j_head).unwrap();

        self.builder.position_at_end(j_exit);
        let ni = self.builder.build_int_add(i, one, "t.mm.ni").unwrap();
        self.builder.build_store(iv, ni).unwrap();
        self.builder.build_unconditional_branch(i_head).unwrap();

        self.builder.position_at_end(i_exit);
        Ok(res.into())
    }

    /// `t.slice(axis, start, end)` — contiguous `[start, end)` band along
    /// one axis, other axes untouched (a copy). The receiver is walked as
    /// `outer × axis_len × inner` (outer = product of dims left of the
    /// axis, inner = product right of it); each outer block keeps its
    /// `[start, end)` middle band. Axis/start/end may be runtime; all
    /// bounds are checked at runtime.
    pub(super) fn compile_tensor_slice(
        &mut self,
        t_ptr: PointerValue<'ctx>,
        args: &[CallArg],
        call_span: &Span,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        if args.len() != 3 {
            return Err(format!(
                "slice takes exactly 3 arguments (axis, start, end), found {}",
                args.len()
            ));
        }
        let elem = self.tensor_transform_elem(call_span)?;
        let elem_size = self.tensor_elem_size(elem)?;
        let i64_t = self.context.i64_type();
        let fn_val = self.current_fn.unwrap();
        let axis = self.compile_expr(&args[0].value)?.into_int_value();
        let start = self.compile_expr(&args[1].value)?.into_int_value();
        let end = self.compile_expr(&args[2].value)?.into_int_value();
        let rank_val = self.tensor_load_rank(t_ptr);
        // axis in [0, rank) — unsigned compare also rejects negatives.
        let axis_oob = self
            .builder
            .build_int_compare(IntPredicate::UGE, axis, rank_val, "t.slc.aoob")
            .unwrap();
        let axis_ok = self.builder.build_not(axis_oob, "t.slc.aok").unwrap();
        self.emit_tensor_guard(axis_ok, "slice axis out of bounds")?;
        // Single pass over the dims: outer (i<axis), axis_len (i==axis),
        // inner (i>axis).
        let outer_s = self.create_entry_alloca(fn_val, "t.slc.outer", i64_t.into());
        let inner_s = self.create_entry_alloca(fn_val, "t.slc.inner", i64_t.into());
        let axislen_s = self.create_entry_alloca(fn_val, "t.slc.axlen", i64_t.into());
        self.builder
            .build_store(outer_s, i64_t.const_int(1, false))
            .unwrap();
        self.builder
            .build_store(inner_s, i64_t.const_int(1, false))
            .unwrap();
        self.builder
            .build_store(axislen_s, i64_t.const_int(1, false))
            .unwrap();
        let iv = self.create_entry_alloca(fn_val, "t.slc.i", i64_t.into());
        self.builder
            .build_store(iv, i64_t.const_int(0, false))
            .unwrap();
        let head = self.context.append_basic_block(fn_val, "t.slc.dh");
        let dbody = self.context.append_basic_block(fn_val, "t.slc.db");
        let dexit = self.context.append_basic_block(fn_val, "t.slc.de");
        self.builder.build_unconditional_branch(head).unwrap();
        self.builder.position_at_end(head);
        let i = self
            .builder
            .build_load(i64_t, iv, "t.slc.iv")
            .unwrap()
            .into_int_value();
        let cont = self
            .builder
            .build_int_compare(IntPredicate::ULT, i, rank_val, "t.slc.cont")
            .unwrap();
        self.builder
            .build_conditional_branch(cont, dbody, dexit)
            .unwrap();
        self.builder.position_at_end(dbody);
        let dslot = self.tensor_dim_slot_dyn(t_ptr, i, "t.slc.dp");
        let d = self
            .builder
            .build_load(i64_t, dslot, "t.slc.d")
            .unwrap()
            .into_int_value();
        let lt = self
            .builder
            .build_int_compare(IntPredicate::ULT, i, axis, "t.slc.lt")
            .unwrap();
        let eq = self
            .builder
            .build_int_compare(IntPredicate::EQ, i, axis, "t.slc.eq")
            .unwrap();
        let gt = self
            .builder
            .build_int_compare(IntPredicate::UGT, i, axis, "t.slc.gt")
            .unwrap();
        let outer = self
            .builder
            .build_load(i64_t, outer_s, "t.slc.outv")
            .unwrap()
            .into_int_value();
        let outer_m = self.builder.build_int_mul(outer, d, "t.slc.outm").unwrap();
        let outer_n = self
            .builder
            .build_select(lt, outer_m, outer, "t.slc.outn")
            .unwrap()
            .into_int_value();
        self.builder.build_store(outer_s, outer_n).unwrap();
        let axlen = self
            .builder
            .build_load(i64_t, axislen_s, "t.slc.alv")
            .unwrap()
            .into_int_value();
        let axlen_n = self
            .builder
            .build_select(eq, d, axlen, "t.slc.aln")
            .unwrap()
            .into_int_value();
        self.builder.build_store(axislen_s, axlen_n).unwrap();
        let inner = self
            .builder
            .build_load(i64_t, inner_s, "t.slc.innv")
            .unwrap()
            .into_int_value();
        let inner_m = self.builder.build_int_mul(inner, d, "t.slc.innm").unwrap();
        let inner_n = self
            .builder
            .build_select(gt, inner_m, inner, "t.slc.innn")
            .unwrap()
            .into_int_value();
        self.builder.build_store(inner_s, inner_n).unwrap();
        let ni = self
            .builder
            .build_int_add(i, i64_t.const_int(1, false), "t.slc.ni")
            .unwrap();
        self.builder.build_store(iv, ni).unwrap();
        self.builder.build_unconditional_branch(head).unwrap();
        self.builder.position_at_end(dexit);
        let outer = self
            .builder
            .build_load(i64_t, outer_s, "t.slc.outer.f")
            .unwrap()
            .into_int_value();
        let inner = self
            .builder
            .build_load(i64_t, inner_s, "t.slc.inner.f")
            .unwrap()
            .into_int_value();
        let axis_len = self
            .builder
            .build_load(i64_t, axislen_s, "t.slc.axlen.f")
            .unwrap()
            .into_int_value();
        // Bounds: 0 <= start <= end <= axis_len.
        let start_neg = self
            .builder
            .build_int_compare(IntPredicate::SLT, start, i64_t.const_zero(), "t.slc.sneg")
            .unwrap();
        let start_ok = self.builder.build_not(start_neg, "t.slc.snn").unwrap();
        self.emit_tensor_guard(start_ok, "slice start must be non-negative")?;
        let end_lt_start = self
            .builder
            .build_int_compare(IntPredicate::SLT, end, start, "t.slc.els")
            .unwrap();
        let order_ok = self.builder.build_not(end_lt_start, "t.slc.ord").unwrap();
        self.emit_tensor_guard(order_ok, "slice end is before start")?;
        let end_oob = self
            .builder
            .build_int_compare(IntPredicate::SGT, end, axis_len, "t.slc.eoob")
            .unwrap();
        let end_ok = self.builder.build_not(end_oob, "t.slc.eok").unwrap();
        self.emit_tensor_guard(end_ok, "slice end out of bounds for the axis")?;
        let band = self
            .builder
            .build_int_sub(end, start, "t.slc.band")
            .unwrap();
        // new_count = outer * band * inner.
        let ob = self.builder.build_int_mul(outer, band, "t.slc.ob").unwrap();
        let new_count = self.builder.build_int_mul(ob, inner, "t.slc.nc").unwrap();
        let src_data = self.tensor_data_ptr_dyn(t_ptr, rank_val, "t.slc.srcd");
        let (res, res_data) = self.tensor_alloc_runtime(rank_val, new_count, elem_size);
        // Header: copy dims, replacing the axis slot with `band`.
        let jv = self.create_entry_alloca(fn_val, "t.slc.j", i64_t.into());
        self.builder
            .build_store(jv, i64_t.const_int(0, false))
            .unwrap();
        let hh = self.context.append_basic_block(fn_val, "t.slc.hh");
        let hb = self.context.append_basic_block(fn_val, "t.slc.hb");
        let he = self.context.append_basic_block(fn_val, "t.slc.he");
        self.builder.build_unconditional_branch(hh).unwrap();
        self.builder.position_at_end(hh);
        let j = self
            .builder
            .build_load(i64_t, jv, "t.slc.jv")
            .unwrap()
            .into_int_value();
        let hcont = self
            .builder
            .build_int_compare(IntPredicate::ULT, j, rank_val, "t.slc.hcont")
            .unwrap();
        self.builder
            .build_conditional_branch(hcont, hb, he)
            .unwrap();
        self.builder.position_at_end(hb);
        let dslot = self.tensor_dim_slot_dyn(t_ptr, j, "t.slc.hdp");
        let dj = self
            .builder
            .build_load(i64_t, dslot, "t.slc.hd")
            .unwrap()
            .into_int_value();
        let is_axis = self
            .builder
            .build_int_compare(IntPredicate::EQ, j, axis, "t.slc.isax")
            .unwrap();
        let written = self
            .builder
            .build_select(is_axis, band, dj, "t.slc.hw")
            .unwrap()
            .into_int_value();
        let rslot = self.tensor_dim_slot_dyn(res, j, "t.slc.rdp");
        self.builder.build_store(rslot, written).unwrap();
        let nj = self
            .builder
            .build_int_add(j, i64_t.const_int(1, false), "t.slc.nj")
            .unwrap();
        self.builder.build_store(jv, nj).unwrap();
        self.builder.build_unconditional_branch(hh).unwrap();
        self.builder.position_at_end(he);
        // Copy: for o in 0..outer, memcpy band*inner elements from
        // src[o*axis_len*inner + start*inner] to dst[o*band*inner].
        let band_inner = self.builder.build_int_mul(band, inner, "t.slc.bi").unwrap();
        let copy_bytes = self
            .builder
            .build_int_mul(band_inner, i64_t.const_int(elem_size, false), "t.slc.cb")
            .unwrap();
        let ov = self.create_entry_alloca(fn_val, "t.slc.o", i64_t.into());
        self.builder
            .build_store(ov, i64_t.const_int(0, false))
            .unwrap();
        let ch = self.context.append_basic_block(fn_val, "t.slc.ch");
        let cb = self.context.append_basic_block(fn_val, "t.slc.cb2");
        let ce = self.context.append_basic_block(fn_val, "t.slc.ce");
        self.builder.build_unconditional_branch(ch).unwrap();
        self.builder.position_at_end(ch);
        let o = self
            .builder
            .build_load(i64_t, ov, "t.slc.ov")
            .unwrap()
            .into_int_value();
        let ccont = self
            .builder
            .build_int_compare(IntPredicate::ULT, o, outer, "t.slc.ccont")
            .unwrap();
        self.builder
            .build_conditional_branch(ccont, cb, ce)
            .unwrap();
        self.builder.position_at_end(cb);
        let ali = self
            .builder
            .build_int_mul(axis_len, inner, "t.slc.ali")
            .unwrap();
        let block = self.builder.build_int_mul(o, ali, "t.slc.blk").unwrap();
        let start_inner = self
            .builder
            .build_int_mul(start, inner, "t.slc.si")
            .unwrap();
        let src_off = self
            .builder
            .build_int_add(block, start_inner, "t.slc.soff")
            .unwrap();
        let dst_off = self
            .builder
            .build_int_mul(o, band_inner, "t.slc.doff")
            .unwrap();
        let src_p = unsafe {
            self.builder
                .build_gep(elem, src_data, &[src_off], "t.slc.sp")
                .unwrap()
        };
        let dst_p = unsafe {
            self.builder
                .build_gep(elem, res_data, &[dst_off], "t.slc.dp2")
                .unwrap()
        };
        self.builder
            .build_memcpy(dst_p, 8, src_p, 8, copy_bytes)
            .map_err(|e| format!("slice data copy failed: {:?}", e))?;
        let no = self
            .builder
            .build_int_add(o, i64_t.const_int(1, false), "t.slc.no")
            .unwrap();
        self.builder.build_store(ov, no).unwrap();
        self.builder.build_unconditional_branch(ch).unwrap();
        self.builder.position_at_end(ce);
        Ok(res.into())
    }

    /// `t.squeeze()` / `t.squeeze(n)` — drop size-1 axes. Data is
    /// unchanged (element count + C-order identical); only the header
    /// shrinks. `squeeze(n)` drops slot `n` (runtime checked == 1);
    /// `squeeze()` drops every size-1 dim. Both build a runtime-rank
    /// result header from the receiver's, then memcpy the data block.
    pub(super) fn compile_tensor_squeeze(
        &mut self,
        t_ptr: PointerValue<'ctx>,
        args: &[CallArg],
        call_span: &Span,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let elem = self.tensor_transform_elem(call_span)?;
        let elem_size = self.tensor_elem_size(elem)?;
        let i64_t = self.context.i64_type();
        let fn_val = self.current_fn.unwrap();
        let rank_val = self.tensor_load_rank(t_ptr);
        let count = self.tensor_count_runtime(t_ptr, rank_val);
        let one = i64_t.const_int(1, false);

        // Decide which slots to keep, and the kept count, with a single
        // pass that writes nothing (just counts); then a second pass
        // writes the kept dims into the freshly-allocated header. `drop_n`
        // (Some axis) for `squeeze(n)`, None for `squeeze()` (drop all
        // size-1).
        let drop_n: Option<IntValue<'ctx>> = match args.len() {
            0 => None,
            1 => {
                let n = self.compile_expr(&args[0].value)?.into_int_value();
                // n in [0, rank) and dims[n] == 1.
                let oob = self
                    .builder
                    .build_int_compare(IntPredicate::UGE, n, rank_val, "t.sqz.oob")
                    .unwrap();
                let in_ok = self.builder.build_not(oob, "t.sqz.in").unwrap();
                self.emit_tensor_guard(in_ok, "squeeze axis out of bounds")?;
                let nslot = self.tensor_dim_slot_dyn(t_ptr, n, "t.sqz.np");
                let nd = self
                    .builder
                    .build_load(i64_t, nslot, "t.sqz.nd")
                    .unwrap()
                    .into_int_value();
                let is_one = self
                    .builder
                    .build_int_compare(IntPredicate::EQ, nd, one, "t.sqz.is1")
                    .unwrap();
                self.emit_tensor_guard(is_one, "cannot squeeze an axis whose size is not 1")?;
                Some(n)
            }
            _ => return Err("squeeze takes 0 or 1 arguments".to_string()),
        };

        // Kept-count: for squeeze(n) it's rank-1; for squeeze() count the
        // non-1 dims.
        let kept_count = match drop_n {
            Some(_) => self
                .builder
                .build_int_sub(rank_val, one, "t.sqz.kc")
                .unwrap(),
            None => {
                let kc = self.create_entry_alloca(fn_val, "t.sqz.kc", i64_t.into());
                self.builder.build_store(kc, i64_t.const_zero()).unwrap();
                let iv = self.create_entry_alloca(fn_val, "t.sqz.ci", i64_t.into());
                self.builder.build_store(iv, i64_t.const_zero()).unwrap();
                let h = self.context.append_basic_block(fn_val, "t.sqz.cnt.h");
                let b = self.context.append_basic_block(fn_val, "t.sqz.cnt.b");
                let e = self.context.append_basic_block(fn_val, "t.sqz.cnt.e");
                self.builder.build_unconditional_branch(h).unwrap();
                self.builder.position_at_end(h);
                let i = self
                    .builder
                    .build_load(i64_t, iv, "t.sqz.cv")
                    .unwrap()
                    .into_int_value();
                let cont = self
                    .builder
                    .build_int_compare(IntPredicate::ULT, i, rank_val, "t.sqz.ccont")
                    .unwrap();
                self.builder.build_conditional_branch(cont, b, e).unwrap();
                self.builder.position_at_end(b);
                let dslot = self.tensor_dim_slot_dyn(t_ptr, i, "t.sqz.cdp");
                let d = self
                    .builder
                    .build_load(i64_t, dslot, "t.sqz.cd")
                    .unwrap()
                    .into_int_value();
                let keep = self
                    .builder
                    .build_int_compare(IntPredicate::NE, d, one, "t.sqz.keep")
                    .unwrap();
                let cur = self
                    .builder
                    .build_load(i64_t, kc, "t.sqz.kcv")
                    .unwrap()
                    .into_int_value();
                let inc = self.builder.build_int_add(cur, one, "t.sqz.kinc").unwrap();
                let next = self
                    .builder
                    .build_select(keep, inc, cur, "t.sqz.ksel")
                    .unwrap()
                    .into_int_value();
                self.builder.build_store(kc, next).unwrap();
                let ni = self.builder.build_int_add(i, one, "t.sqz.cni").unwrap();
                self.builder.build_store(iv, ni).unwrap();
                self.builder.build_unconditional_branch(h).unwrap();
                self.builder.position_at_end(e);
                self.builder
                    .build_load(i64_t, kc, "t.sqz.kc.f")
                    .unwrap()
                    .into_int_value()
            }
        };

        let src_data = self.tensor_data_ptr_dyn(t_ptr, rank_val, "t.sqz.src");
        let (res, res_data) = self.tensor_alloc_runtime(kept_count, count, elem_size);

        // Write kept dims: walk receiver dims, append each kept dim to a
        // running output cursor. `keep(j)` = (drop_n is None ? dims[j]!=1
        // : j != drop_n).
        let outc = self.create_entry_alloca(fn_val, "t.sqz.out", i64_t.into());
        self.builder.build_store(outc, i64_t.const_zero()).unwrap();
        let jv = self.create_entry_alloca(fn_val, "t.sqz.j", i64_t.into());
        self.builder.build_store(jv, i64_t.const_zero()).unwrap();
        let wh = self.context.append_basic_block(fn_val, "t.sqz.wh");
        let wb = self.context.append_basic_block(fn_val, "t.sqz.wb");
        let wkeep = self.context.append_basic_block(fn_val, "t.sqz.wkeep");
        let wskip = self.context.append_basic_block(fn_val, "t.sqz.wskip");
        let we = self.context.append_basic_block(fn_val, "t.sqz.we");
        self.builder.build_unconditional_branch(wh).unwrap();
        self.builder.position_at_end(wh);
        let j = self
            .builder
            .build_load(i64_t, jv, "t.sqz.wjv")
            .unwrap()
            .into_int_value();
        let wcont = self
            .builder
            .build_int_compare(IntPredicate::ULT, j, rank_val, "t.sqz.wcont")
            .unwrap();
        self.builder
            .build_conditional_branch(wcont, wb, we)
            .unwrap();
        self.builder.position_at_end(wb);
        let dslot = self.tensor_dim_slot_dyn(t_ptr, j, "t.sqz.wdp");
        let dj = self
            .builder
            .build_load(i64_t, dslot, "t.sqz.wd")
            .unwrap()
            .into_int_value();
        let keep = match drop_n {
            Some(n) => self
                .builder
                .build_int_compare(IntPredicate::NE, j, n, "t.sqz.wkeepc")
                .unwrap(),
            None => self
                .builder
                .build_int_compare(IntPredicate::NE, dj, one, "t.sqz.wkeepc")
                .unwrap(),
        };
        self.builder
            .build_conditional_branch(keep, wkeep, wskip)
            .unwrap();
        self.builder.position_at_end(wkeep);
        let cur = self
            .builder
            .build_load(i64_t, outc, "t.sqz.outv")
            .unwrap()
            .into_int_value();
        let rslot = self.tensor_dim_slot_dyn(res, cur, "t.sqz.wrp");
        self.builder.build_store(rslot, dj).unwrap();
        let nout = self.builder.build_int_add(cur, one, "t.sqz.nout").unwrap();
        self.builder.build_store(outc, nout).unwrap();
        self.builder.build_unconditional_branch(wskip).unwrap();
        self.builder.position_at_end(wskip);
        let nj = self.builder.build_int_add(j, one, "t.sqz.wnj").unwrap();
        self.builder.build_store(jv, nj).unwrap();
        self.builder.build_unconditional_branch(wh).unwrap();
        self.builder.position_at_end(we);
        // Copy data unchanged.
        let bytes = self
            .builder
            .build_int_mul(count, i64_t.const_int(elem_size, false), "t.sqz.bytes")
            .unwrap();
        self.builder
            .build_memcpy(res_data, 8, src_data, 8, bytes)
            .map_err(|e| format!("squeeze data copy failed: {:?}", e))?;
        Ok(res.into())
    }

    /// `t.iter_axis(n)` — axis iteration. Yields the `dims[n]`
    /// sub-tensors obtained by fixing the index along axis `n` (the axis
    /// is *dropped* — NumPy `take(i, axis=n)` semantics) as a `Vec` of
    /// *copies*. A rank-1 receiver yields the scalar elements directly
    /// (`Vec[T]`); a rank ≥ 2 receiver yields `Vec[Tensor[T,
    /// dims-with-slot-n-removed]]`. The receiver `rank` is static (the
    /// typechecker rejects splice/bare-`S` shapes) and `elem` is the
    /// receiver's element type; everything else (dims, axis) is read /
    /// computed at runtime, so `?`-dims and a runtime axis work. The
    /// result `Vec` is built directly as a `{ptr, len, cap}` value; its
    /// per-element tensor blocks are freed by the `Vec[Tensor]` cleanup
    /// arm (`track_vec_of_tensors_var`). Interpreter twin:
    /// `eval_tensor_iter_axis`.
    pub(super) fn compile_tensor_iter_axis(
        &mut self,
        t_ptr: PointerValue<'ctx>,
        elem: BasicTypeEnum<'ctx>,
        rank: usize,
        args: &[CallArg],
        _span: &Span,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        if args.len() != 1 {
            return Err(format!(
                "iter_axis takes exactly 1 argument (the axis), found {}",
                args.len()
            ));
        }
        let elem_size = self.tensor_elem_size(elem)?;
        let i64_t = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let fn_val = self.current_fn.unwrap();
        let axis = self.compile_expr(&args[0].value)?.into_int_value();
        let rank_const = i64_t.const_int(rank as u64, false);
        // axis in [0, rank) — unsigned compare rejects negatives too.
        let oob = self
            .builder
            .build_int_compare(IntPredicate::UGE, axis, rank_const, "t.ia.oob")
            .unwrap();
        let ok = self.builder.build_not(oob, "t.ia.ok").unwrap();
        self.emit_tensor_guard(ok, "iter_axis axis out of bounds")?;

        let rdims: Vec<IntValue<'ctx>> =
            (0..rank).map(|i| self.tensor_load_dim(t_ptr, i)).collect();
        let src_data = self.tensor_data_ptr(t_ptr, rank, "t.ia.src");

        if rank == 1 {
            // Rank-1: result is `Vec[T]` — a copy of the data.
            let n = rdims[0];
            let bytes = self
                .builder
                .build_int_mul(n, i64_t.const_int(elem_size, false), "t.ia.bytes")
                .unwrap();
            let buf = self
                .builder
                .build_call(self.malloc_fn, &[bytes.into()], "t.ia.buf")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_pointer_value();
            self.builder
                .build_memcpy(buf, 8, src_data, 8, bytes)
                .map_err(|e| format!("iter_axis data copy failed: {:?}", e))?;
            return Ok(self.build_vec_value(buf, n, n));
        }

        // Rank ≥ 2. Single pass: outer (i<axis), n_buckets (i==axis),
        // inner (i>axis).
        let mut outer = i64_t.const_int(1, false);
        let mut inner = i64_t.const_int(1, false);
        let mut n_buckets = i64_t.const_int(1, false);
        for (i, &d) in rdims.iter().enumerate() {
            let ci = i64_t.const_int(i as u64, false);
            let lt = self
                .builder
                .build_int_compare(IntPredicate::ULT, ci, axis, "t.ia.lt")
                .unwrap();
            let eq = self
                .builder
                .build_int_compare(IntPredicate::EQ, ci, axis, "t.ia.eq")
                .unwrap();
            let gt = self
                .builder
                .build_int_compare(IntPredicate::UGT, ci, axis, "t.ia.gt")
                .unwrap();
            let outer_m = self.builder.build_int_mul(outer, d, "t.ia.outm").unwrap();
            outer = self
                .builder
                .build_select(lt, outer_m, outer, "t.ia.out")
                .unwrap()
                .into_int_value();
            n_buckets = self
                .builder
                .build_select(eq, d, n_buckets, "t.ia.nb")
                .unwrap()
                .into_int_value();
            let inner_m = self.builder.build_int_mul(inner, d, "t.ia.innm").unwrap();
            inner = self
                .builder
                .build_select(gt, inner_m, inner, "t.ia.inn")
                .unwrap()
                .into_int_value();
        }
        let sub_rank = rank - 1;
        // sub_dims[k] = (k < axis) ? rdims[k] : rdims[k+1].
        let sub_dims: Vec<IntValue<'ctx>> = (0..sub_rank)
            .map(|k| {
                let ck = i64_t.const_int(k as u64, false);
                let lt = self
                    .builder
                    .build_int_compare(IntPredicate::ULT, ck, axis, "t.ia.sdlt")
                    .unwrap();
                self.builder
                    .build_select(lt, rdims[k], rdims[k + 1], "t.ia.sd")
                    .unwrap()
                    .into_int_value()
            })
            .collect();

        // Result buffer: `n_buckets` tensor pointers (8 bytes each).
        let buf_bytes = self
            .builder
            .build_int_mul(n_buckets, i64_t.const_int(8, false), "t.ia.bufb")
            .unwrap();
        let result_buf = self
            .builder
            .build_call(self.malloc_fn, &[buf_bytes.into()], "t.ia.rbuf")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();

        let inner_bytes = self
            .builder
            .build_int_mul(inner, i64_t.const_int(elem_size, false), "t.ia.ib")
            .unwrap();
        let sub_header_bytes = i64_t.const_int(8 * (1 + sub_rank as u64), false);

        // Bucket loop: for b in 0..n_buckets.
        let bv = self.create_entry_alloca(fn_val, "t.ia.b", i64_t.into());
        self.builder.build_store(bv, i64_t.const_zero()).unwrap();
        let bh = self.context.append_basic_block(fn_val, "t.ia.bh");
        let bb = self.context.append_basic_block(fn_val, "t.ia.bb");
        let be = self.context.append_basic_block(fn_val, "t.ia.be");
        self.builder.build_unconditional_branch(bh).unwrap();
        self.builder.position_at_end(bh);
        let b = self
            .builder
            .build_load(i64_t, bv, "t.ia.bv")
            .unwrap()
            .into_int_value();
        let bcont = self
            .builder
            .build_int_compare(IntPredicate::ULT, b, n_buckets, "t.ia.bcont")
            .unwrap();
        self.builder
            .build_conditional_branch(bcont, bb, be)
            .unwrap();
        self.builder.position_at_end(bb);
        // Allocate the sub-tensor: header (sub_rank + sub_dims) + data.
        let bucket_len = self.builder.build_int_mul(outer, inner, "t.ia.bl").unwrap();
        let bucket_data_bytes = self
            .builder
            .build_int_mul(bucket_len, i64_t.const_int(elem_size, false), "t.ia.bdb")
            .unwrap();
        let sub_total = self
            .builder
            .build_int_add(sub_header_bytes, bucket_data_bytes, "t.ia.subt")
            .unwrap();
        let sub_t = self
            .builder
            .build_call(self.malloc_fn, &[sub_total.into()], "t.ia.subt.a")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        self.builder
            .build_store(sub_t, i64_t.const_int(sub_rank as u64, false))
            .unwrap();
        for (k, dv) in sub_dims.iter().enumerate() {
            let slot = self.tensor_header_slot(sub_t, 1 + k as u64, &format!("t.ia.sd{}.p", k));
            self.builder.build_store(slot, *dv).unwrap();
        }
        let sub_data = self.tensor_data_ptr(sub_t, sub_rank, "t.ia.subd");
        // Gather: for o in 0..outer, memcpy `inner` elements from
        // src[(o*n_buckets + b)*inner] to sub_data[o*inner].
        let ov = self.create_entry_alloca(fn_val, "t.ia.o", i64_t.into());
        self.builder.build_store(ov, i64_t.const_zero()).unwrap();
        let oh = self.context.append_basic_block(fn_val, "t.ia.oh");
        let ob = self.context.append_basic_block(fn_val, "t.ia.ob");
        let oe = self.context.append_basic_block(fn_val, "t.ia.oe");
        self.builder.build_unconditional_branch(oh).unwrap();
        self.builder.position_at_end(oh);
        let o = self
            .builder
            .build_load(i64_t, ov, "t.ia.ov")
            .unwrap()
            .into_int_value();
        let ocont = self
            .builder
            .build_int_compare(IntPredicate::ULT, o, outer, "t.ia.ocont")
            .unwrap();
        self.builder
            .build_conditional_branch(ocont, ob, oe)
            .unwrap();
        self.builder.position_at_end(ob);
        let on = self.builder.build_int_mul(o, n_buckets, "t.ia.on").unwrap();
        let onb = self.builder.build_int_add(on, b, "t.ia.onb").unwrap();
        let src_off = self.builder.build_int_mul(onb, inner, "t.ia.soff").unwrap();
        let dst_off = self.builder.build_int_mul(o, inner, "t.ia.doff").unwrap();
        let src_p = unsafe {
            self.builder
                .build_gep(elem, src_data, &[src_off], "t.ia.sp")
                .unwrap()
        };
        let dst_p = unsafe {
            self.builder
                .build_gep(elem, sub_data, &[dst_off], "t.ia.dp")
                .unwrap()
        };
        self.builder
            .build_memcpy(dst_p, 8, src_p, 8, inner_bytes)
            .map_err(|e| format!("iter_axis bucket copy failed: {:?}", e))?;
        let no = self
            .builder
            .build_int_add(o, i64_t.const_int(1, false), "t.ia.no")
            .unwrap();
        self.builder.build_store(ov, no).unwrap();
        self.builder.build_unconditional_branch(oh).unwrap();
        self.builder.position_at_end(oe);
        // Store the sub-tensor pointer into result_buf[b].
        let bp = unsafe {
            self.builder
                .build_gep(ptr_ty, result_buf, &[b], "t.ia.bp")
                .unwrap()
        };
        self.builder.build_store(bp, sub_t).unwrap();
        let nb = self
            .builder
            .build_int_add(b, i64_t.const_int(1, false), "t.ia.nbk")
            .unwrap();
        self.builder.build_store(bv, nb).unwrap();
        self.builder.build_unconditional_branch(bh).unwrap();
        self.builder.position_at_end(be);
        Ok(self.build_vec_value(result_buf, n_buckets, n_buckets))
    }

    /// B-2026-07-13-7: deep-clone a whole tensor heap block. A tensor value is
    /// a single pointer to a `[i64 rank][i64 dims…][data]` block; a `Vec[Tensor]`
    /// (the `iter_axis` result) stores those pointers, so a `let r = rows[i]`
    /// element move-out shallow-copies the 8-byte pointer and the binding's drop
    /// races the container's per-element drop (`track_vec_of_tensors_var`) on the
    /// SAME block — a double-free. This emits a
    /// `karac_clone_<mangled>(src: *ptr-slot, dst: *ptr-slot)` clone fn (the shape
    /// the `emit_clone_fn_for_type_expr` dispatcher expects) that reads the source
    /// block pointer, allocates a byte-identical copy — rank and dims read from
    /// the header, so any rank / dynamic dims work — and stores the fresh pointer
    /// into `dst`. `None` when the element type can't be resolved (a splice-shape
    /// element, never an `iter_axis` sub-tensor, whose dims are concrete).
    pub(super) fn emit_tensor_clone_fn(&mut self, te: &TypeExpr) -> Option<FunctionValue<'ctx>> {
        // Element size from the tensor's element type (first type arg).
        let TypeKind::Path(p) = &te.kind else {
            return None;
        };
        let elem_te = p.generic_args.as_ref()?.iter().find_map(|a| match a {
            GenericArg::Type(t) => Some(t.clone()),
            _ => None,
        })?;
        let elem_ll = self.llvm_type_for_type_expr(&elem_te);
        let elem_size = self.tensor_elem_size(elem_ll).ok()?;

        let type_name = Self::display_mangle_te(te);
        let fn_name = format!("karac_clone_{type_name}");
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();

        let saved_bb = self.builder.get_insert_block();
        let clone_fn_ty = self
            .context
            .void_type()
            .fn_type(&[ptr_ty.into(), ptr_ty.into()], false);
        let fn_val = self.module.add_function(
            &fn_name,
            clone_fn_ty,
            Some(inkwell::module::Linkage::Internal),
        );

        let entry = self.context.append_basic_block(fn_val, "entry");
        let null_bb = self.context.append_basic_block(fn_val, "t.cl.null");
        let work_bb = self.context.append_basic_block(fn_val, "t.cl.work");
        let ph = self.context.append_basic_block(fn_val, "t.cl.ph");
        let pb = self.context.append_basic_block(fn_val, "t.cl.pb");
        let pe = self.context.append_basic_block(fn_val, "t.cl.pe");

        self.builder.position_at_end(entry);
        let src = fn_val.get_nth_param(0).unwrap().into_pointer_value();
        let dst = fn_val.get_nth_param(1).unwrap().into_pointer_value();
        let t = self
            .builder
            .build_load(ptr_ty, src, "t.cl.t")
            .unwrap()
            .into_pointer_value();
        let is_null = self.builder.build_is_null(t, "t.cl.isnull").unwrap();
        self.builder
            .build_conditional_branch(is_null, null_bb, work_bb)
            .unwrap();

        // Null source (move-out sentinel) → store null, done.
        self.builder.position_at_end(null_bb);
        self.builder.build_store(dst, ptr_ty.const_null()).unwrap();
        self.builder.build_return(None).unwrap();

        // rank = header[0]; product of dims via a runtime loop over 0..rank.
        self.builder.position_at_end(work_bb);
        let rank = self
            .builder
            .build_load(i64_t, t, "t.cl.rank")
            .unwrap()
            .into_int_value();
        let prod_slot = self.create_entry_alloca(fn_val, "t.cl.prod", i64_t.into());
        let idx_slot = self.create_entry_alloca(fn_val, "t.cl.i", i64_t.into());
        self.builder
            .build_store(prod_slot, i64_t.const_int(1, false))
            .unwrap();
        self.builder
            .build_store(idx_slot, i64_t.const_zero())
            .unwrap();
        self.builder.build_unconditional_branch(ph).unwrap();

        self.builder.position_at_end(ph);
        let i = self
            .builder
            .build_load(i64_t, idx_slot, "t.cl.iv")
            .unwrap()
            .into_int_value();
        let cont = self
            .builder
            .build_int_compare(IntPredicate::ULT, i, rank, "t.cl.cont")
            .unwrap();
        self.builder.build_conditional_branch(cont, pb, pe).unwrap();

        self.builder.position_at_end(pb);
        let slot_idx = self
            .builder
            .build_int_add(i, i64_t.const_int(1, false), "t.cl.si")
            .unwrap();
        let dim_p = unsafe {
            self.builder
                .build_gep(i64_t, t, &[slot_idx], "t.cl.dimp")
                .unwrap()
        };
        let dim = self
            .builder
            .build_load(i64_t, dim_p, "t.cl.dim")
            .unwrap()
            .into_int_value();
        let prod = self
            .builder
            .build_load(i64_t, prod_slot, "t.cl.pv")
            .unwrap()
            .into_int_value();
        let prod2 = self.builder.build_int_mul(prod, dim, "t.cl.pm").unwrap();
        self.builder.build_store(prod_slot, prod2).unwrap();
        let ni = self
            .builder
            .build_int_add(i, i64_t.const_int(1, false), "t.cl.ni")
            .unwrap();
        self.builder.build_store(idx_slot, ni).unwrap();
        self.builder.build_unconditional_branch(ph).unwrap();

        self.builder.position_at_end(pe);
        let prod = self
            .builder
            .build_load(i64_t, prod_slot, "t.cl.prodf")
            .unwrap()
            .into_int_value();
        // total = 8*(1 + rank) header + elem_size*prod data.
        let hdr_words = self
            .builder
            .build_int_add(rank, i64_t.const_int(1, false), "t.cl.hw")
            .unwrap();
        let hdr_bytes = self
            .builder
            .build_int_mul(hdr_words, i64_t.const_int(8, false), "t.cl.hb")
            .unwrap();
        let data_bytes = self
            .builder
            .build_int_mul(prod, i64_t.const_int(elem_size, false), "t.cl.db")
            .unwrap();
        let total = self
            .builder
            .build_int_add(hdr_bytes, data_bytes, "t.cl.tot")
            .unwrap();
        let new = self
            .builder
            .build_call(self.malloc_fn, &[total.into()], "t.cl.new")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        self.builder.build_memcpy(new, 8, t, 8, total).unwrap();
        self.builder.build_store(dst, new).unwrap();
        self.builder.build_return(None).unwrap();

        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        Some(fn_val)
    }

    /// Build a `{ptr, len, cap}` Vec value from a buffer pointer + len +
    /// cap (the layout `vec_struct_type` produces).
    pub(super) fn build_vec_value(
        &self,
        buf: PointerValue<'ctx>,
        len: IntValue<'ctx>,
        cap: IntValue<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        let vec_ty = self.vec_struct_type();
        let mut agg = vec_ty.get_undef();
        agg = self
            .builder
            .build_insert_value(agg, buf, 0, "t.vec.data")
            .unwrap()
            .into_struct_value();
        agg = self
            .builder
            .build_insert_value(agg, len, 1, "t.vec.len")
            .unwrap()
            .into_struct_value();
        agg = self
            .builder
            .build_insert_value(agg, cap, 2, "t.vec.cap")
            .unwrap()
            .into_struct_value();
        agg.into()
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

    /// Cross-argument `?`-dim equality asserts at a call boundary
    /// (design.md § Runtime equality check, the call-boundary flavor).
    ///
    /// When two of a generic callee's `Tensor` parameters share a named
    /// `Dim` parameter — e.g. `K` in `matmul(a: Tensor[T, [M, K]], b:
    /// Tensor[T, [K, N]])` — the two argument dims that bind `K` must be
    /// equal at runtime. The type system can't prove equality of two `?`
    /// dims statically (both are dynamic), so the compiler inserts a check
    /// that fails fast with a clear message rather than letting the callee
    /// read out of bounds. Concrete-vs-concrete is resolved at type-check
    /// time (E_SHAPE on mismatch — no code here); concrete-vs-`?` lowers to
    /// a bounds check against the static value (foldable by the optimizer);
    /// `?`-vs-`?` lowers to a full equality check.
    ///
    /// Lives only on the generic-call path: a named dim parameter can only
    /// appear in a function with generic params, so a non-generic call
    /// never has cross-argument dim constraints. The tensor pointers come
    /// from the already-compiled `arg_vals` (a tensor value is a single
    /// pointer), so this reads no variable slots and is safe to run at any
    /// point after the arguments are compiled.
    pub(super) fn emit_tensor_crossarg_dim_asserts(
        &mut self,
        generic_fn: &crate::ast::Function,
        args: &[CallArg],
        arg_vals: &[BasicValueEnum<'ctx>],
    ) -> Result<(), String> {
        // Only the callee's own generic params are Dim params that create a
        // cross-argument constraint — a module-level integer constant used
        // as a dim is concrete (the typechecker checks it against the arg
        // directly), not a shared runtime dim, so it must not be grouped.
        let generic_names: std::collections::HashSet<&str> = generic_fn
            .generic_params
            .as_ref()
            .map(|gp| gp.params.iter().map(|p| p.name.as_str()).collect())
            .unwrap_or_default();
        if generic_names.is_empty() {
            return Ok(());
        }

        // dim-param name → the (param index, dim index) positions binding
        // it. BTreeMap keeps the emitted asserts in a deterministic order
        // (stable IR across builds).
        let mut by_name: std::collections::BTreeMap<String, Vec<(usize, usize)>> =
            std::collections::BTreeMap::new();
        for (pi, p) in generic_fn.params.iter().enumerate() {
            if let Some(named) = self.tensor_param_named_dims(&p.ty, &generic_names) {
                for (di, slot) in named.iter().enumerate() {
                    if let Some(nm) = slot {
                        by_name.entry(nm.clone()).or_default().push((pi, di));
                    }
                }
            }
        }

        let i64_t = self.context.i64_type();
        for (dim_name, positions) in by_name {
            if positions.len() < 2 {
                continue;
            }
            // Resolve each position to its argument's tensor pointer, the
            // dim slot, the statically-known dim value (if any), and a
            // human label. A position whose argument didn't compile to a
            // pointer (shouldn't happen for a tensor arg) is skipped — never
            // a false trap.
            let mut resolved: Vec<(PointerValue<'ctx>, usize, Option<i64>, String)> = Vec::new();
            for (pi, di) in positions {
                let Some(BasicValueEnum::PointerValue(ptr)) = arg_vals.get(pi).copied() else {
                    continue;
                };
                let static_val = self.tensor_arg_static_dim(args.get(pi), di);
                let label = match args.get(pi).map(|a| &a.value.kind) {
                    Some(ExprKind::Identifier(n)) => format!("argument '{}'", n),
                    _ => format!("argument {}", pi),
                };
                resolved.push((ptr, di, static_val, label));
            }
            if resolved.len() < 2 {
                continue;
            }

            // A statically-known value on any position is the witness — the
            // typechecker guarantees all concrete values in a group agree,
            // so every other (runtime) position is bounds-checked against it
            // (the foldable concrete-vs-`?` flavor). With no static witness,
            // all positions are `?`: pin them to the first and assert the
            // rest equal it.
            let static_witness = resolved.iter().find_map(|(_, _, sv, _)| *sv);
            if let Some(d) = static_witness {
                let want = i64_t.const_int(d as u64, false);
                for (ptr, di, sv, label) in &resolved {
                    if sv.is_some() {
                        continue;
                    }
                    let got = self.tensor_load_dim(*ptr, *di);
                    let ok = self
                        .builder
                        .build_int_compare(IntPredicate::EQ, got, want, "t.kdim.ok")
                        .unwrap();
                    self.emit_tensor_guard(
                        ok,
                        &format!(
                            "call to {}: shape mismatch — dim '{}' of {} (dim {}) must be {}",
                            generic_fn.name, dim_name, label, di, d
                        ),
                    )?;
                }
            } else {
                let (ref_ptr, ref_di, _, ref_label) = resolved[0].clone();
                let reference = self.tensor_load_dim(ref_ptr, ref_di);
                for (ptr, di, _, label) in &resolved[1..] {
                    let got = self.tensor_load_dim(*ptr, *di);
                    let ok = self
                        .builder
                        .build_int_compare(IntPredicate::EQ, got, reference, "t.kdim.ok")
                        .unwrap();
                    self.emit_tensor_guard(
                        ok,
                        &format!(
                            "call to {}: shape mismatch — dim '{}' differs between arguments \
                             ({} dim {} vs {} dim {})",
                            generic_fn.name, dim_name, ref_label, ref_di, label, di
                        ),
                    )?;
                }
            }
        }
        Ok(())
    }

    /// Named `Dim` parameter per dim slot of a `Tensor` parameter type
    /// (`Some(name)` for a bare-identifier dim that is one of the callee's
    /// generic params, `None` otherwise). `None` for the whole type when it
    /// is not a `Tensor` (after peeling one `ref`/`mut ref`) or when its
    /// shape carries a `...` splice (rank unknown — out of scope here). A
    /// concrete literal, a `?`, or a non-identifier const expression each
    /// map to `None`: none impose a cross-argument equality constraint.
    fn tensor_param_named_dims(
        &self,
        te: &TypeExpr,
        generic_names: &std::collections::HashSet<&str>,
    ) -> Option<Vec<Option<String>>> {
        let inner = match &te.kind {
            TypeKind::Ref(i) | TypeKind::MutRef(i) => i.as_ref(),
            _ => te,
        };
        let TypeKind::Path(path) = &inner.kind else {
            return None;
        };
        if path.segments.last().map(|s| s.as_str()) != Some("Tensor") {
            return None;
        }
        let gargs = path.generic_args.as_ref()?;
        for ga in gargs {
            if let GenericArg::Shape(shape) = ga {
                let mut out = Vec::with_capacity(shape.dims.len());
                for d in &shape.dims {
                    match d {
                        ShapeDim::Const(e) => match &e.kind {
                            ExprKind::Identifier(name) if generic_names.contains(name.as_str()) => {
                                out.push(Some(name.clone()))
                            }
                            _ => out.push(None),
                        },
                        ShapeDim::Dynamic { .. } => out.push(None),
                        ShapeDim::Splice { .. } => return None,
                    }
                }
                return Some(out);
            }
        }
        None
    }

    /// Statically-known dim `di` of a call argument, if any — from the
    /// argument's tensor-var info (identifier binding) or the lowering
    /// side-table keyed by the argument's span (any other tensor-typed
    /// expression). `None` when the dim is `?` / runtime, or the argument
    /// is not a tracked tensor.
    fn tensor_arg_static_dim(&self, arg: Option<&CallArg>, di: usize) -> Option<i64> {
        let arg = arg?;
        if let ExprKind::Identifier(n) = &arg.value.kind {
            if let Some(info) = self.tensor_var_infos.get(n.as_str()) {
                return info.dims.get(di).copied().flatten();
            }
        }
        let key = (arg.value.span.offset, arg.value.span.length);
        self.tensor_typed_exprs
            .get(&key)
            .and_then(|ti| ti.dims.get(di).copied().flatten())
    }

    /// Free `data` when `cap > 0` (temporary `{ptr,len,cap}` disposal).
    /// `elem_abi_size` is the element ABI size for the `karac_free_buf`
    /// recycling hint (`cap × elem_abi_size`; phase-10 line 282) — pass the
    /// exact `sizeof(T)` so a mid-size multi-byte-element buffer clears the 1
    /// MiB cache fast-reject, `1` for a String / when the element is unknown (a
    /// sound under-hint, never a correctness issue), or `0` to ask the allocator.
    pub(super) fn emit_free_if_cap_positive(
        &mut self,
        data: PointerValue<'ctx>,
        cap: IntValue<'ctx>,
        elem_abi_size: u64,
    ) {
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
        self.emit_free_buf_call(data, cap, elem_abi_size);
        self.builder.build_unconditional_branch(join_bb).unwrap();
        self.builder.position_at_end(join_bb);
    }

    // ── Element-wise arithmetic ─────────────────────────────────

    /// True iff `expr`'s result is a tensor (its span is in the lowering
    /// side-table). Used to tell the tensor operand of a tensor⊕scalar op
    /// from the scalar one.
    fn expr_is_tensor_typed(&self, expr: &Expr) -> bool {
        self.tensor_typed_exprs
            .contains_key(&(expr.span.offset, expr.span.length))
    }

    /// Is a tensor *operand* of an element-wise op a fresh owned temporary
    /// this op must free after copying out of it? `a + b + c`'s inner `a + b`
    /// is malloc'd, owned by nothing else, and read once here. Tensor
    /// arithmetic (`Binary`) and negation (`Unary`-`Neg`) always produce a
    /// fresh owned block; everything else defers to the receiver rule (which
    /// keeps borrowed identifier / `ref`-return operands un-freed).
    fn tensor_operand_is_owned_fresh_temp(&self, expr: &Expr) -> bool {
        match &expr.kind {
            ExprKind::Binary { .. } => true,
            ExprKind::Unary {
                op: crate::ast::UnaryOp::Neg,
                ..
            } => true,
            _ => self.tensor_receiver_is_owned_fresh_temp(expr),
        }
    }

    /// True iff both operand exprs have fully-static, identical tensor
    /// shapes (every dim a known literal, same rank, same values). When so,
    /// the typechecker has already proved shape agreement and the runtime
    /// shape guard is dead. A `?` (runtime) dim on either side returns false.
    fn tensor_operand_dims_statically_equal(&self, left: &Expr, right: &Expr) -> bool {
        let lkey = (left.span.offset, left.span.length);
        let rkey = (right.span.offset, right.span.length);
        let (Some(l), Some(r)) = (
            self.tensor_typed_exprs.get(&lkey),
            self.tensor_typed_exprs.get(&rkey),
        ) else {
            return false;
        };
        l.dims.len() == r.dims.len()
            && l.dims
                .iter()
                .zip(&r.dims)
                .all(|(a, b)| matches!((a, b), (Some(x), Some(y)) if x == y))
    }

    /// Copy `rank` dim words from `src`'s header into `dst`'s (slot 1
    /// onward). `dst`'s rank slot 0 is already written by
    /// `tensor_alloc_runtime`.
    fn tensor_copy_header_dims(
        &mut self,
        src: PointerValue<'ctx>,
        dst: PointerValue<'ctx>,
        rank_val: IntValue<'ctx>,
    ) {
        let i64_t = self.context.i64_type();
        let src_dims = self.tensor_header_slot(src, 1, "t.cph.src");
        let dst_dims = self.tensor_header_slot(dst, 1, "t.cph.dst");
        let bytes = self
            .builder
            .build_int_mul(rank_val, i64_t.const_int(8, false), "t.cph.bytes")
            .unwrap();
        self.builder
            .build_memcpy(dst_dims, 8, src_dims, 8, bytes)
            .unwrap();
    }

    /// Runtime shape-equality guard between two tensors: rank then every
    /// dim. The typechecker already proved concrete-vs-concrete dims equal;
    /// this catches `?`-dim mismatches (and the `run_program` bypass). Traps
    /// with the same message as the interpreter twin.
    fn emit_tensor_shape_eq_guard(
        &mut self,
        a: PointerValue<'ctx>,
        b: PointerValue<'ctx>,
    ) -> Result<(), String> {
        let i64_t = self.context.i64_type();
        let ra = self.tensor_load_rank(a);
        let rb = self.tensor_load_rank(b);
        let rank_eq = self
            .builder
            .build_int_compare(IntPredicate::EQ, ra, rb, "t.bin.rankeq")
            .unwrap();
        self.emit_tensor_guard(rank_eq, "tensor shape mismatch in element-wise operator")?;
        // for i in 0..rank { assert a.dim[i] == b.dim[i] }
        let fn_val = self.current_fn.unwrap();
        let iv = self.create_entry_alloca(fn_val, "t.bin.di", i64_t.into());
        self.builder
            .build_store(iv, i64_t.const_int(0, false))
            .unwrap();
        let head = self.context.append_basic_block(fn_val, "t.bin.dhead");
        let body = self.context.append_basic_block(fn_val, "t.bin.dbody");
        let exit = self.context.append_basic_block(fn_val, "t.bin.dexit");
        self.builder.build_unconditional_branch(head).unwrap();
        self.builder.position_at_end(head);
        let i = self
            .builder
            .build_load(i64_t, iv, "t.bin.div")
            .unwrap()
            .into_int_value();
        let cont = self
            .builder
            .build_int_compare(IntPredicate::ULT, i, ra, "t.bin.dcont")
            .unwrap();
        self.builder
            .build_conditional_branch(cont, body, exit)
            .unwrap();
        self.builder.position_at_end(body);
        let da = {
            let slot = self.tensor_dim_slot_dyn(a, i, "t.bin.dap");
            self.builder
                .build_load(i64_t, slot, "t.bin.da")
                .unwrap()
                .into_int_value()
        };
        let db = {
            let slot = self.tensor_dim_slot_dyn(b, i, "t.bin.dbp");
            self.builder
                .build_load(i64_t, slot, "t.bin.db")
                .unwrap()
                .into_int_value()
        };
        let dim_eq = self
            .builder
            .build_int_compare(IntPredicate::EQ, da, db, "t.bin.dimeq")
            .unwrap();
        self.emit_tensor_guard(dim_eq, "tensor shape mismatch in element-wise operator")?;
        let ni = self
            .builder
            .build_int_add(i, i64_t.const_int(1, false), "t.bin.dni")
            .unwrap();
        self.builder.build_store(iv, ni).unwrap();
        self.builder.build_unconditional_branch(head).unwrap();
        self.builder.position_at_end(exit);
        Ok(())
    }

    /// The per-element loop for an element-wise op. `a_data` is the left
    /// (C-order) tensor's data. For tensor⊕tensor `b_data` is the right
    /// tensor's data; for tensor⊕scalar `scalar` holds the broadcast value
    /// (`scalar_on_left` puts it on the operator's left, e.g. `2 - t`). Thin
    /// adapter over the shared kernel
    /// [`emit_elementwise_map`](super::Codegen::emit_elementwise_map) (dense —
    /// no bitmaps); each element pair routes through `compile_binop_typed`, so
    /// the per-element op inherits the exact scalar semantics — int overflow
    /// trap, div-by-zero trap, signed/unsigned division — matching the
    /// interpreter.
    #[allow(clippy::too_many_arguments)]
    fn emit_tensor_binop_loop(
        &mut self,
        op: &BinOp,
        elem: BasicTypeEnum<'ctx>,
        count: IntValue<'ctx>,
        res_data: PointerValue<'ctx>,
        a_data: PointerValue<'ctx>,
        b_data: Option<PointerValue<'ctx>>,
        scalar: Option<BasicValueEnum<'ctx>>,
        is_unsigned: bool,
        scalar_on_left: bool,
    ) -> Result<(), String> {
        let lhs = ContainerAccess {
            data: a_data,
            len: count,
            elem,
            unsigned: is_unsigned,
            bitmap: None,
        };
        let other = match (b_data, scalar) {
            (Some(bd), _) => MapOther::Access(ContainerAccess {
                data: bd,
                len: count,
                elem,
                unsigned: is_unsigned,
                bitmap: None,
            }),
            (None, Some(s)) => MapOther::Scalar {
                value: s,
                on_left: scalar_on_left,
            },
            (None, None) => return Err("tensor binop loop: no second operand".to_string()),
        };
        let dest = MapDest {
            data: res_data,
            elem,
            bitmap: None,
        };
        self.emit_elementwise_map(&lhs, &other, &MapKernelOp::Binop(op), &dest)
    }

    /// Element-wise `Tensor ⊕ Tensor` / `Tensor ⊕ scalar` for `+ - * /`.
    /// Routed from `compile_expr`'s `Binary` arm when the result span is
    /// tensor-typed. Mallocs a fresh value-semantics result; both operands
    /// are read (a fresh-temp operand is freed after the copy so `a + b + c`
    /// intermediates don't leak). The result's `FreeTensor` cleanup is
    /// registered by the let-binding site from the same side-table entry.
    pub(super) fn compile_tensor_binop(
        &mut self,
        op: &BinOp,
        left: &Expr,
        right: &Expr,
        binary_span: &Span,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let key = (binary_span.offset, binary_span.length);
        let ti = self
            .tensor_typed_exprs
            .get(&key)
            .ok_or_else(|| {
                "tensor binary-op result type is not statically known \
                 (missing lowering side-table entry)"
                    .to_string()
            })?
            .clone();
        let elem = self.llvm_type_for_type_expr(&ti.elem);
        let elem_size = self.tensor_elem_size(elem)?;
        let is_unsigned = type_expr_is_unsigned_int(&ti.elem);

        let left_is_tensor = self.expr_is_tensor_typed(left);
        let right_is_tensor = self.expr_is_tensor_typed(right);

        let lhs_val = self.compile_expr(left)?;
        let rhs_val = self.compile_expr(right)?;

        let result = if left_is_tensor && right_is_tensor {
            let lptr = lhs_val.into_pointer_value();
            let rptr = rhs_val.into_pointer_value();
            // Skip the runtime shape-equality guard when both operands have
            // fully-static, identical shapes — the typechecker already proved
            // them equal (E_SHAPE otherwise), so the guard would be dead. Any
            // `?` dim on either side keeps the guard.
            if !self.tensor_operand_dims_statically_equal(left, right) {
                self.emit_tensor_shape_eq_guard(lptr, rptr)?;
            }
            let rank = self.tensor_load_rank(lptr);
            let count = self.tensor_count_runtime(lptr, rank);
            let (res, res_data) = self.tensor_alloc_runtime(rank, count, elem_size);
            self.tensor_copy_header_dims(lptr, res, rank);
            let l_data = self.tensor_data_ptr_dyn(lptr, rank, "t.bin.ld");
            let r_data = self.tensor_data_ptr_dyn(rptr, rank, "t.bin.rd");
            self.emit_tensor_binop_loop(
                op,
                elem,
                count,
                res_data,
                l_data,
                Some(r_data),
                None,
                is_unsigned,
                false,
            )?;
            res
        } else {
            let (tptr, scalar, scalar_on_left) = if left_is_tensor {
                (lhs_val.into_pointer_value(), rhs_val, false)
            } else {
                (rhs_val.into_pointer_value(), lhs_val, true)
            };
            let rank = self.tensor_load_rank(tptr);
            let count = self.tensor_count_runtime(tptr, rank);
            let (res, res_data) = self.tensor_alloc_runtime(rank, count, elem_size);
            self.tensor_copy_header_dims(tptr, res, rank);
            let t_data = self.tensor_data_ptr_dyn(tptr, rank, "t.bin.td");
            self.emit_tensor_binop_loop(
                op,
                elem,
                count,
                res_data,
                t_data,
                None,
                Some(scalar),
                is_unsigned,
                scalar_on_left,
            )?;
            res
        };

        // Free fresh-temporary operands (intermediates owned by nothing else).
        if left_is_tensor && self.tensor_operand_is_owned_fresh_temp(left) {
            self.builder
                .build_call(self.free_fn, &[lhs_val.into_pointer_value().into()], "")
                .unwrap();
        }
        if right_is_tensor && self.tensor_operand_is_owned_fresh_temp(right) {
            self.builder
                .build_call(self.free_fn, &[rhs_val.into_pointer_value().into()], "")
                .unwrap();
        }

        Ok(result.into())
    }

    /// Element-wise negation `-t` — a fresh tensor with each element negated
    /// via the kernel's `MapKernelOp::Neg` (the scalar `-x` semantics: IEEE
    /// `fneg` for floats — `-0.0` for `0.0`, matching the interpreter's `-f`;
    /// B-2026-07-01-1 — and checked `0 - x` for ints, so `i64::MIN` traps
    /// like `checked_neg`). The operand is read; a fresh-temp operand is
    /// freed after the copy.
    pub(super) fn compile_tensor_neg(
        &mut self,
        operand: &Expr,
        unary_span: &Span,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let key = (unary_span.offset, unary_span.length);
        let ti = self
            .tensor_typed_exprs
            .get(&key)
            .ok_or_else(|| {
                "tensor negation result type is not statically known \
                 (missing lowering side-table entry)"
                    .to_string()
            })?
            .clone();
        let elem = self.llvm_type_for_type_expr(&ti.elem);
        let elem_size = self.tensor_elem_size(elem)?;
        let is_unsigned = type_expr_is_unsigned_int(&ti.elem);

        let tptr = self.compile_expr(operand)?.into_pointer_value();
        let rank = self.tensor_load_rank(tptr);
        let count = self.tensor_count_runtime(tptr, rank);
        let (res, res_data) = self.tensor_alloc_runtime(rank, count, elem_size);
        self.tensor_copy_header_dims(tptr, res, rank);
        let t_data = self.tensor_data_ptr_dyn(tptr, rank, "t.neg.td");
        let lhs = ContainerAccess {
            data: t_data,
            len: count,
            elem,
            unsigned: is_unsigned,
            bitmap: None,
        };
        let dest = MapDest {
            data: res_data,
            elem,
            bitmap: None,
        };
        self.emit_elementwise_map(&lhs, &MapOther::Unary, &MapKernelOp::Neg, &dest)?;
        if self.tensor_operand_is_owned_fresh_temp(operand) {
            self.builder
                .build_call(self.free_fn, &[tptr.into()], "")
                .unwrap();
        }
        Ok(res.into())
    }

    // ── Broadcasting ────────────────────────────────────────────

    /// Dispatch a tensor broadcasting method (`broadcast_add` / `_sub` /
    /// `_mul` / `_div`) — NumPy-style element-wise op where the argument's
    /// shape is broadcast against the receiver's (size-1 dims expand; shapes
    /// align from the right). Phase-11 "Explicit broadcasting methods".
    ///
    /// **Identifier receiver only** (like reductions): the element type +
    /// static rank come from the name-keyed `tensor_var_infos`, immune to the
    /// parser's postfix span reuse. A value / chained receiver stamps the
    /// *call* span onto the receiver, so the span-keyed side-table holds the
    /// result shape there, not the receiver's — those stay on `karac run`
    /// (bind to a `let` first). The *argument* may be any tensor-typed expr:
    /// its span doesn't collide with the call span, so its static rank is read
    /// from `tensor_var_infos` (identifier) or the span-keyed side-table.
    /// `None` when the method isn't a broadcast, the receiver isn't a tensor
    /// identifier, or the argument's rank isn't statically known.
    pub(super) fn try_compile_tensor_broadcast(
        &mut self,
        object: &Expr,
        method: &str,
        args: &[CallArg],
        _call_span: &Span,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        let op = match method {
            "broadcast_add" => BinOp::Add,
            "broadcast_sub" => BinOp::Sub,
            "broadcast_mul" => BinOp::Mul,
            "broadcast_div" => BinOp::Div,
            _ => return Ok(None),
        };
        let ExprKind::Identifier(name) = &object.kind else {
            return Ok(None);
        };
        let Some(info) = self.tensor_var_infos.get(name.as_str()).cloned() else {
            return Ok(None);
        };
        if args.len() != 1 {
            return Err(format!(
                "{method} takes exactly 1 argument, found {}",
                args.len()
            ));
        }
        let arg = &args[0].value;
        // The argument's static rank — from the name-keyed registry (identifier
        // arg) or the span-keyed side-table (any other tensor-typed expr; the
        // arg span doesn't collide with the call span). Needed to unroll the
        // alignment; `None` (unknown rank) falls through to `karac run`.
        let arg_rank = match &arg.kind {
            ExprKind::Identifier(an) => {
                self.tensor_var_infos.get(an.as_str()).map(|i| i.dims.len())
            }
            _ => self
                .tensor_typed_exprs
                .get(&(arg.span.offset, arg.span.length))
                .map(|ti| ti.dims.len()),
        };
        let Some(arg_rank) = arg_rank else {
            return Ok(None);
        };
        let a_ptr = self.tensor_ptr_for_var(name)?;
        let b_ptr = self.compile_expr(arg)?.into_pointer_value();
        let result = self.compile_tensor_broadcast(
            &op,
            info.elem,
            info.elem_unsigned,
            info.dims.len(),
            a_ptr,
            arg_rank,
            b_ptr,
        )?;
        // Free a fresh-temp argument (e.g. `a.broadcast_add(b + c)`); the
        // identifier receiver is a live binding and is never freed here.
        if self.tensor_operand_is_owned_fresh_temp(arg) {
            self.builder
                .build_call(self.free_fn, &[b_ptr.into()], "")
                .unwrap();
        }
        Ok(Some(result))
    }

    /// C-order strides for a runtime dim list: `stride[i] = product(dims[i+1..])`,
    /// `stride[last] = 1`. Pure IR over already-loaded dim values.
    fn runtime_c_strides(&self, dims: &[IntValue<'ctx>]) -> Vec<IntValue<'ctx>> {
        let i64_t = self.context.i64_type();
        let n = dims.len();
        let mut strides = vec![i64_t.const_int(1, false); n];
        for i in (0..n.saturating_sub(1)).rev() {
            strides[i] = self
                .builder
                .build_int_mul(strides[i + 1], dims[i + 1], "t.cstride")
                .unwrap();
        }
        strides
    }

    /// Lower `self <op> other` with NumPy-style broadcasting into a fresh
    /// result tensor. Static ranks `ra`/`rb` unroll the per-axis alignment;
    /// every dim *value* comes from the runtime headers (so `?` dims and
    /// runtime broadcasting work uniformly). Per output axis: the aligned dim
    /// pair must be broadcast-compatible (equal, or one is 1) — guarded at
    /// runtime — and the output dim is their max. Effective per-operand
    /// strides are 0 on absent / size-1 axes, so a single C-order pass over
    /// the output reads the correct (possibly repeated) source element from
    /// each operand. Interpreter twin: `eval_tensor_broadcast`.
    #[allow(clippy::too_many_arguments)]
    fn compile_tensor_broadcast(
        &mut self,
        op: &BinOp,
        elem: BasicTypeEnum<'ctx>,
        is_unsigned: bool,
        ra: usize,
        a_ptr: PointerValue<'ctx>,
        rb: usize,
        b_ptr: PointerValue<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let fn_val = self.current_fn.unwrap();
        let i64_t = self.context.i64_type();
        let elem_size = self.tensor_elem_size(elem)?;
        let one = i64_t.const_int(1, false);
        let zero = i64_t.const_zero();
        let out_rank = ra.max(rb);
        let off_a = out_rank - ra;
        let off_b = out_rank - rb;

        // Operand dims from the headers, then within-operand C-order strides.
        let a_dims: Vec<IntValue<'ctx>> = (0..ra).map(|j| self.tensor_load_dim(a_ptr, j)).collect();
        let b_dims: Vec<IntValue<'ctx>> = (0..rb).map(|j| self.tensor_load_dim(b_ptr, j)).collect();
        let a_strides = self.runtime_c_strides(&a_dims);
        let b_strides = self.runtime_c_strides(&b_dims);

        // Per output axis: compatibility guard, output dim (max), and the
        // effective per-operand strides (0 where the axis is absent / size-1).
        let mut out_dims: Vec<IntValue<'ctx>> = Vec::with_capacity(out_rank);
        let mut eff_a: Vec<IntValue<'ctx>> = Vec::with_capacity(out_rank);
        let mut eff_b: Vec<IntValue<'ctx>> = Vec::with_capacity(out_rank);
        for k in 0..out_rank {
            let da = if k >= off_a { a_dims[k - off_a] } else { one };
            let db = if k >= off_b { b_dims[k - off_b] } else { one };
            let eq = self
                .builder
                .build_int_compare(IntPredicate::EQ, da, db, "t.bc.eq")
                .unwrap();
            let a1 = self
                .builder
                .build_int_compare(IntPredicate::EQ, da, one, "t.bc.a1")
                .unwrap();
            let b1 = self
                .builder
                .build_int_compare(IntPredicate::EQ, db, one, "t.bc.b1")
                .unwrap();
            let or1 = self.builder.build_or(eq, a1, "t.bc.or1").unwrap();
            let ok = self.builder.build_or(or1, b1, "t.bc.ok").unwrap();
            self.emit_tensor_guard(ok, "shapes are not broadcast-compatible")?;
            let agtb = self
                .builder
                .build_int_compare(IntPredicate::UGT, da, db, "t.bc.gt")
                .unwrap();
            let od = self
                .builder
                .build_select(agtb, da, db, "t.bc.od")
                .unwrap()
                .into_int_value();
            out_dims.push(od);
            let ea = if k >= off_a {
                let j = k - off_a;
                let is1 = self
                    .builder
                    .build_int_compare(IntPredicate::EQ, a_dims[j], one, "t.bc.ea1")
                    .unwrap();
                self.builder
                    .build_select(is1, zero, a_strides[j], "t.bc.ea")
                    .unwrap()
                    .into_int_value()
            } else {
                zero
            };
            eff_a.push(ea);
            let eb = if k >= off_b {
                let j = k - off_b;
                let is1 = self
                    .builder
                    .build_int_compare(IntPredicate::EQ, b_dims[j], one, "t.bc.eb1")
                    .unwrap();
                self.builder
                    .build_select(is1, zero, b_strides[j], "t.bc.eb")
                    .unwrap()
                    .into_int_value()
            } else {
                zero
            };
            eff_b.push(eb);
        }

        // Output strides + element count.
        let out_strides = self.runtime_c_strides(&out_dims);
        let mut count = one;
        for d in &out_dims {
            count = self.builder.build_int_mul(count, *d, "t.bc.cnt").unwrap();
        }

        // Allocate the result and write its dim header.
        let out_rank_val = i64_t.const_int(out_rank as u64, false);
        let (res, res_data) = self.tensor_alloc_runtime(out_rank_val, count, elem_size);
        for (k, dv) in out_dims.iter().enumerate() {
            let slot = self.tensor_header_slot(res, 1 + k as u64, &format!("t.bc.hd{k}"));
            self.builder.build_store(slot, *dv).unwrap();
        }

        let a_data = self.tensor_data_ptr(a_ptr, ra, "t.bc.ad");
        let b_data = self.tensor_data_ptr(b_ptr, rb, "t.bc.bd");

        // for f in 0..count { coords via out_strides; fa/fb via eff strides;
        //                     res[f] = a[fa] op b[fb] }
        let fv = self.create_entry_alloca(fn_val, "t.bc.f", i64_t.into());
        self.builder.build_store(fv, zero).unwrap();
        let head = self.context.append_basic_block(fn_val, "t.bc.head");
        let body = self.context.append_basic_block(fn_val, "t.bc.body");
        let exit = self.context.append_basic_block(fn_val, "t.bc.exit");
        self.builder.build_unconditional_branch(head).unwrap();
        self.builder.position_at_end(head);
        let f = self
            .builder
            .build_load(i64_t, fv, "t.bc.fv")
            .unwrap()
            .into_int_value();
        let cont = self
            .builder
            .build_int_compare(IntPredicate::ULT, f, count, "t.bc.cont")
            .unwrap();
        self.builder
            .build_conditional_branch(cont, body, exit)
            .unwrap();
        self.builder.position_at_end(body);
        let mut fa = zero;
        let mut fb = zero;
        for k in 0..out_rank {
            let div = self
                .builder
                .build_int_unsigned_div(f, out_strides[k], "t.bc.div")
                .unwrap();
            let coord = self
                .builder
                .build_int_unsigned_rem(div, out_dims[k], "t.bc.coord")
                .unwrap();
            let ta = self
                .builder
                .build_int_mul(coord, eff_a[k], "t.bc.ta")
                .unwrap();
            fa = self.builder.build_int_add(fa, ta, "t.bc.fa").unwrap();
            let tb = self
                .builder
                .build_int_mul(coord, eff_b[k], "t.bc.tb")
                .unwrap();
            fb = self.builder.build_int_add(fb, tb, "t.bc.fb").unwrap();
        }
        let ap = unsafe {
            self.builder
                .build_gep(elem, a_data, &[fa], "t.bc.ap")
                .unwrap()
        };
        let av = self.builder.build_load(elem, ap, "t.bc.av").unwrap();
        let bp = unsafe {
            self.builder
                .build_gep(elem, b_data, &[fb], "t.bc.bp")
                .unwrap()
        };
        let bv = self.builder.build_load(elem, bp, "t.bc.bv").unwrap();
        let r = self.compile_binop_typed(op, av, bv, is_unsigned)?;
        let rp = unsafe {
            self.builder
                .build_gep(elem, res_data, &[f], "t.bc.rp")
                .unwrap()
        };
        self.builder.build_store(rp, r).unwrap();
        let nf = self.builder.build_int_add(f, one, "t.bc.nf").unwrap();
        self.builder.build_store(fv, nf).unwrap();
        self.builder.build_unconditional_branch(head).unwrap();
        self.builder.position_at_end(exit);
        Ok(res.into())
    }

    // ── Reductions ──────────────────────────────────────────────

    /// Dispatch a tensor reduction (`sum`/`mean`/`prod`/`min`/`max` →
    /// scalar; `sum_axis`/`mean_axis` → rank-1-lower tensor), phase-11 line
    /// 47 Slice B.
    ///
    /// **Identifier receivers only** (like `iter_axis`): the element type +
    /// rank come from the name-keyed `tensor_var_infos`, which is immune to
    /// the parser's postfix span reuse. A value / chained receiver
    /// (`(a + b).sum()`, `a.reshape(..).sum()`) stamps the *call* span onto
    /// the receiver, so the span-keyed side-table holds the reduce's *result*
    /// type (a scalar, or the rank-1-lower axis tensor) at the receiver key —
    /// the receiver's own element type is unrecoverable there. Those stay on
    /// `karac run`; bind the receiver to a `let` first. `None` when the
    /// method isn't a reduce or the receiver isn't a tensor identifier.
    pub(super) fn try_compile_tensor_reduce(
        &mut self,
        object: &Expr,
        method: &str,
        args: &[CallArg],
        call_span: &Span,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        let is_full = matches!(method, "sum" | "mean" | "prod" | "min" | "max" | "range");
        let is_axis = matches!(method, "sum_axis" | "mean_axis");
        let is_fold = method == "fold";
        let is_map = method == "map";
        let is_zip = method == "zip_with";
        let is_argord = matches!(method, "argmin" | "argmax");
        let is_sort = matches!(method, "sorted" | "argsort");
        if !is_full && !is_axis && !is_fold && !is_map && !is_zip && !is_argord && !is_sort {
            return Ok(None);
        }
        // S6c-12 slice 2: accept a `self` receiver (`ExprKind::SelfValue`) too,
        // so a user `impl Trait for Tensor[T, S]` body may call the builtin
        // reductions on `self` — the self param is typed `ref Tensor[T, S]`, so
        // its element/shape info is registered under `"self"` in
        // `tensor_var_infos`. (Column twin: `try_compile_column_method`.)
        let name = match &object.kind {
            ExprKind::Identifier(n) => n.as_str(),
            ExprKind::SelfValue => "self",
            // Non-identifier receiver — a tensor-producing method chain such as
            // `a.zip_with(b, f).sum()` (B-2026-07-13-5 legs A/C). `tensor_var_infos`
            // is keyed by binding name and has no entry for a temp; the element
            // type comes from `temp_recv_elem_types` (recorded by the typechecker
            // at the reduction call span, span-collision-immune). Handled for the
            // scalar FULL reductions; other forms fall through.
            _ => return self.try_compile_tensor_reduce_expr_receiver(object, method, call_span),
        };
        let Some(info) = self.tensor_var_infos.get(name).cloned() else {
            return Ok(None);
        };
        let t_ptr = self.tensor_ptr_for_var(name)?;
        let result = if is_fold {
            self.compile_tensor_fold(info.elem, info.elem_unsigned, t_ptr, args)?
        } else if is_map {
            self.compile_tensor_map(info.elem, info.elem_unsigned, t_ptr, args)?
        } else if is_zip {
            self.compile_tensor_zip_with(info.elem, info.elem_unsigned, t_ptr, args)?
        } else if is_argord {
            self.compile_tensor_argminmax(info.elem, info.elem_unsigned, t_ptr, method == "argmax")?
        } else if is_sort {
            self.compile_tensor_sort(method, info.elem, info.elem_unsigned, t_ptr)?
        } else if is_full {
            self.compile_tensor_full_reduce(method, info.elem, info.elem_unsigned, t_ptr)?
        } else {
            self.compile_tensor_axis_reduce(
                method,
                info.elem,
                info.elem_unsigned,
                info.dims.len(),
                t_ptr,
                args,
            )?
        };
        Ok(Some(result))
    }

    /// Scalar tensor reduction on a NON-IDENTIFIER receiver — a
    /// tensor-producing method chain (`a.zip_with(b, f).sum()`,
    /// `t.map(g).mean()`), B-2026-07-13-5 legs A/C. The receiver has no
    /// `tensor_var_infos` entry (that table is binding-name-keyed), and its
    /// element type is unrecoverable from the span via `tensor_typed_exprs`
    /// because `MethodCall.span == receiver.span` collapses the reduce / chain
    /// / base spans into one (so the span holds the outer scalar reduce result,
    /// not the intermediate Tensor). The element `TypeExpr` was therefore
    /// recorded span-collision-immune by the typechecker in
    /// `temp_recv_elem_types` at the reduction call span. Compile the receiver
    /// to a fresh tensor pointer, run the scalar reduction (shape read from the
    /// runtime header — only `elem`/`elem_unsigned` are needed here), then free
    /// the temp (a chain result is a fresh block owned by nothing —
    /// `tensor_operand_is_owned_fresh_temp`). Only the scalar full reductions
    /// (`sum`/`mean`/`prod`/`min`/`max`) are wired; the tensor-producing /
    /// axis / arg-order forms on a chained receiver return `None` and fall
    /// through to the existing loud diagnostic.
    fn try_compile_tensor_reduce_expr_receiver(
        &mut self,
        object: &Expr,
        method: &str,
        call_span: &Span,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        if !matches!(method, "sum" | "mean" | "prod" | "min" | "max") {
            return Ok(None);
        }
        let key = (call_span.offset, call_span.length);
        // An ITERATOR-chain terminal (`vec![…].iter().sum()`) records its
        // element type in `iter_terminal_elem_types` at this span; a genuine
        // Tensor reduction never does (a Tensor `.sum()` is typed by the tensor
        // path, not the Iterator protocol). Since the parser collapses a chain's
        // call spans onto one key, a Vec iter source now also carries a
        // `temp_recv_elem_types` entry at this span (B-2026-07-18-39) — without
        // this guard the tensor path would misread that Vec as a Tensor and fail
        // on `.iter()`. Decline so the iterator-chain sum/min/max intercept
        // (which handles the Vec source) gets it.
        if self.iter_terminal_elem_types.contains_key(&key) {
            return Ok(None);
        }
        let Some(elem_te) = self.temp_recv_elem_types.get(&key).cloned() else {
            return Ok(None);
        };
        let elem = self.llvm_type_for_type_expr(&elem_te);
        let elem_unsigned = type_expr_is_unsigned_int(&elem_te);
        // Fusion: a `<base>.zip_with(other, |a,b| ..).<reduce>()` or
        // `<base>.map(|x| ..).<reduce>()` chain folds in one accumulating pass
        // with NO intermediate products tensor (`emit_fused_map_reduce`). Only
        // fires for the recognized inline-closure shapes over a tensor-binding
        // base; everything else falls through to the materialize path below.
        if let Some(fused) = self.try_emit_fused_map_reduce(object, method, elem, elem_unsigned)? {
            return Ok(Some(fused));
        }
        let t_val = self.compile_expr(object)?;
        let t_ptr = t_val.into_pointer_value();
        let result = self.compile_tensor_full_reduce(method, elem, elem_unsigned, t_ptr)?;
        // The chained receiver is a fresh heap tensor owned by nothing; free it
        // after the reduction has read it (mirrors the binop fresh-temp free).
        if self.tensor_operand_is_owned_fresh_temp(object) {
            self.builder
                .build_call(self.free_fn, &[t_ptr.into()], "")
                .unwrap();
        }
        Ok(Some(result))
    }

    /// If `object` is `<base>.zip_with(<other>, |a, b| ..)` or
    /// `<base>.map(|x| ..)` with an INLINE closure literal and `<base>` is a
    /// tensor binding (identifier / `self`), fuse it with this scalar
    /// `sum`/`mean`/`prod` reduce into ONE accumulating loop — the products
    /// never materialize ([`emit_fused_map_reduce`](Self::emit_fused_map_reduce)).
    /// `result_elem`/`result_unsigned` are the intermediate (closure-return)
    /// element type the caller already resolved from `temp_recv_elem_types`.
    ///
    /// Returns `Ok(None)` — falling back to the materialize-then-reduce path —
    /// for any shape not recognized (non-inline closure, non-binding base,
    /// `min`/`max`, wrong arity), so it can never regress a currently-compiling
    /// program. The base is required to be a tensor *binding* so its element
    /// type + data pointer resolve without recursively materializing a sub-chain
    /// (a `map(...).map(...).sum()` inner map still materializes — only the
    /// outermost `map`/`zip_with` fuses with the reduce).
    fn try_emit_fused_map_reduce(
        &mut self,
        object: &Expr,
        method: &str,
        result_elem: BasicTypeEnum<'ctx>,
        result_unsigned: bool,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        let op = match method {
            "sum" => ReduceOp::Sum,
            "mean" => ReduceOp::Mean,
            "prod" => ReduceOp::Prod,
            _ => return Ok(None),
        };
        let ExprKind::MethodCall {
            object: base,
            method: inner,
            args,
            ..
        } = &object.kind
        else {
            return Ok(None);
        };
        let is_zip = inner == "zip_with";
        let is_map = inner == "map";
        if !is_zip && !is_map {
            return Ok(None);
        }
        // Base must be a tensor binding so its element type + pointer resolve
        // without recursively materializing a sub-chain.
        let base_name = match &base.kind {
            ExprKind::Identifier(n) => n.as_str(),
            ExprKind::SelfValue => "self",
            _ => return Ok(None),
        };
        let Some(info) = self.tensor_var_infos.get(base_name).cloned() else {
            return Ok(None);
        };
        // The inline closure is the last arg; the zip partner is arg 0.
        let arity = if is_zip { 2 } else { 1 };
        if args.len() != arity {
            return Ok(None);
        }
        let ExprKind::Closure { params, body, .. } = &args[arity - 1].value.kind else {
            return Ok(None);
        };
        if params.len() != arity {
            return Ok(None);
        }

        let base_ptr = self.tensor_ptr_for_var(base_name)?;
        let rank = self.tensor_load_rank(base_ptr);
        let count = self.tensor_count_runtime(base_ptr, rank);

        // Empty policy: a Tensor scalar reduction traps on empty (matches
        // `compile_tensor_full_reduce`'s unconditional guard), so guard BEFORE
        // the fused loop; `emit_fused_map_reduce` then assumes non-empty.
        let i64_t = self.context.i64_type();
        let nonempty = self
            .builder
            .build_int_compare(IntPredicate::UGT, count, i64_t.const_zero(), "fmr.ne")
            .unwrap();
        self.emit_tensor_guard(nonempty, "cannot reduce an empty tensor")?;

        let base_data = self.tensor_data_ptr_dyn(base_ptr, rank, "fmr.base.d");
        let lhs = ContainerAccess {
            data: base_data,
            len: count,
            elem: info.elem,
            unsigned: info.elem_unsigned,
            bitmap: None,
        };
        // `zip_with`'s partner is another `Tensor[T, S]` of the SAME element
        // type T (its signature forces it), so it shares `info.elem`. Compile it
        // to a pointer and shape-guard against the base — the exact guard
        // `compile_tensor_zip_with` emits for the materialize path.
        let other_access = if is_zip {
            let other_ptr = self.compile_expr(&args[0].value)?.into_pointer_value();
            self.emit_tensor_shape_eq_guard(base_ptr, other_ptr)?;
            let other_rank = self.tensor_load_rank(other_ptr);
            let other_data = self.tensor_data_ptr_dyn(other_ptr, other_rank, "fmr.other.d");
            Some(ContainerAccess {
                data: other_data,
                len: count,
                elem: info.elem,
                unsigned: info.elem_unsigned,
                bitmap: None,
            })
        } else {
            None
        };

        let result = self.emit_fused_map_reduce(
            &lhs,
            other_access.as_ref(),
            (params, body),
            result_elem,
            result_unsigned,
            op,
        )?;
        Ok(Some(result))
    }

    /// `fold[A](init, |acc, elem| body) -> A` — the general left-fold
    /// primitive (mirror of `compile_column_fold`, but a tensor has no
    /// validity bitmap, so EVERY element folds and there is no per-slot gate).
    /// Inlines the closure body into an in-place reduction loop over the C-order
    /// element buffer, threading an `A`-typed accumulator; captures resolve
    /// through the enclosing scope, and the two closure params (`acc`, `elem`)
    /// bind as locals (shadowed outer bindings saved/restored). An EMPTY tensor
    /// returns `init` unchanged — the loop simply doesn't run (the fold
    /// identity, NO trap, unlike the fixed reductions).
    ///
    /// First cut is POD-only + inline-literal-only, exactly like `Column.fold`:
    /// a closure-valued local / named fn or a heap / aggregate accumulator is
    /// rejected LOUDLY (each works under `karac run`). Tensor elements are
    /// always numeric (no `String` element to guard).
    fn compile_tensor_fold(
        &mut self,
        elem: BasicTypeEnum<'ctx>,
        _unsigned: bool,
        t_ptr: PointerValue<'ctx>,
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        if args.len() != 2 {
            return Err(format!(
                "Tensor.fold expects 2 arguments (init, closure), got {}",
                args.len()
            ));
        }
        let ExprKind::Closure { params, body, .. } = &args[1].value.kind else {
            return Err(
                "Tensor.fold expects an inline closure literal as its second \
                        argument under `karac build`; a closure-valued local / named \
                        fn is not yet supported by the native backend (it works under \
                        `karac run`)."
                    .to_string(),
            );
        };
        if params.len() != 2 {
            return Err(format!(
                "Tensor.fold closure must take exactly 2 parameters (acc, elem), got {}",
                params.len()
            ));
        }

        // Seed the accumulator; its LLVM type IS `A`. Heap / aggregate `A`
        // (String / Vec struct / pointer) is rejected — no drop plumbing.
        let init_val = self.compile_expr(&args[0].value)?;
        let acc_ty = init_val.get_type();
        if acc_ty.is_struct_type() || acc_ty.is_pointer_type() || acc_ty.is_array_type() {
            return Err(
                "Tensor.fold with a heap / aggregate accumulator is not yet \
                        supported by the native backend (`karac build`); use a scalar \
                        accumulator, or run it under `karac run`."
                    .to_string(),
            );
        }

        let fn_val = self.current_fn.unwrap();
        let i64_t = self.context.i64_type();

        let acc_slot = self.create_entry_alloca(fn_val, "t.fold.acc", acc_ty);
        self.builder.build_store(acc_slot, init_val).unwrap();

        // Element buffer + count (no bitmap — every element is valid).
        let rank = self.tensor_load_rank(t_ptr);
        let count = self.tensor_count_runtime(t_ptr, rank);
        let data = self.tensor_data_ptr_dyn(t_ptr, rank, "t.fold.data");

        // Loop scaffold:
        //   head  → i < count ? apply : exit
        //   apply → elem = data[i]; bind (acc, elem); acc = <closure>; → next
        //   next  → i++ ; → head
        //   exit  → ret acc
        let head = self.context.append_basic_block(fn_val, "t.fold.head");
        let apply_bb = self.context.append_basic_block(fn_val, "t.fold.apply");
        let next_bb = self.context.append_basic_block(fn_val, "t.fold.next");
        let exit_bb = self.context.append_basic_block(fn_val, "t.fold.exit");

        let i_slot = self.create_entry_alloca(fn_val, "t.fold.i", i64_t.into());
        self.builder
            .build_store(i_slot, i64_t.const_zero())
            .unwrap();
        self.builder.build_unconditional_branch(head).unwrap();

        // head: i < count ?
        self.builder.position_at_end(head);
        let i = self
            .builder
            .build_load(i64_t, i_slot, "t.fold.i.load")
            .unwrap()
            .into_int_value();
        let in_range = self
            .builder
            .build_int_compare(IntPredicate::ULT, i, count, "t.fold.ir")
            .unwrap();
        self.builder
            .build_conditional_branch(in_range, apply_bb, exit_bb)
            .unwrap();

        // apply: elem = data[i]; bind closure params; compile body; store acc.
        self.builder.position_at_end(apply_bb);
        let elem_addr = unsafe {
            self.builder
                .build_in_bounds_gep(elem, data, &[i], "t.fold.ep")
                .unwrap()
        };
        let elem_val = self
            .builder
            .build_load(elem, elem_addr, "t.fold.ev")
            .unwrap();
        let acc_cur = self
            .builder
            .build_load(acc_ty, acc_slot, "t.fold.acc.cur")
            .unwrap();

        let pname = |i: usize| match &params[i].pattern.kind {
            PatternKind::Binding(n) => n.clone(),
            _ => format!("_t_fold_p{i}"),
        };
        let acc_name = pname(0);
        let elem_name = pname(1);
        let saved_acc = self.variables.get(&acc_name).copied();
        let saved_elem = self.variables.get(&elem_name).copied();

        let acc_param = self.create_entry_alloca(fn_val, &acc_name, acc_ty);
        self.builder.build_store(acc_param, acc_cur).unwrap();
        self.variables.insert(
            acc_name.clone(),
            VarSlot {
                ptr: acc_param,
                ty: acc_ty,
            },
        );
        let elem_param = self.create_entry_alloca(fn_val, &elem_name, elem);
        self.builder.build_store(elem_param, elem_val).unwrap();
        self.variables.insert(
            elem_name.clone(),
            VarSlot {
                ptr: elem_param,
                ty: elem,
            },
        );

        let new_acc = self.compile_expr(body)?;
        let new_acc = self.coerce_scalar_to_type(new_acc, acc_ty);

        match saved_elem {
            Some(s) => {
                self.variables.insert(elem_name.clone(), s);
            }
            None => {
                self.variables.remove(&elem_name);
            }
        }
        match saved_acc {
            Some(s) => {
                self.variables.insert(acc_name.clone(), s);
            }
            None => {
                self.variables.remove(&acc_name);
            }
        }

        self.builder.build_store(acc_slot, new_acc).unwrap();
        self.builder.build_unconditional_branch(next_bb).unwrap();

        // next: i++ ; loop.
        self.builder.position_at_end(next_bb);
        let i2 = self
            .builder
            .build_load(i64_t, i_slot, "t.fold.i.load2")
            .unwrap()
            .into_int_value();
        let inc = self
            .builder
            .build_int_add(i2, i64_t.const_int(1, false), "t.fold.i.inc")
            .unwrap();
        self.builder.build_store(i_slot, inc).unwrap();
        self.builder.build_unconditional_branch(head).unwrap();

        // exit: the threaded accumulator.
        self.builder.position_at_end(exit_bb);
        Ok(self
            .builder
            .build_load(acc_ty, acc_slot, "t.fold.result")
            .unwrap())
    }

    /// `map(|x| ...) -> Tensor[T, ...S]` — element-wise map producing a fresh
    /// tensor of the same shape (S6c-2). Parity with `Column.map` minus the
    /// validity gate: a tensor has no null concept, so EVERY C-order element is
    /// mapped. Allocates a value-semantics result, copies the source header
    /// dims, and inlines the closure body per element via the shared
    /// `emit_elementwise_map` (dense). Same first-cut boundaries — inline
    /// closure literal only; tensor elements are always numeric (no `String`
    /// element to guard).
    fn compile_tensor_map(
        &mut self,
        elem: BasicTypeEnum<'ctx>,
        unsigned: bool,
        t_ptr: PointerValue<'ctx>,
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        if args.len() != 1 {
            return Err(format!(
                "Tensor.map expects 1 argument (closure), got {}",
                args.len()
            ));
        }
        let ExprKind::Closure { params, body, .. } = &args[0].value.kind else {
            return Err(
                "Tensor.map expects an inline closure literal under `karac build`; a \
                 closure-valued local / named fn is not yet supported by the native \
                 backend (it works under `karac run`)."
                    .to_string(),
            );
        };
        if params.len() != 1 {
            return Err(format!(
                "Tensor.map closure must take exactly 1 parameter (elem), got {}",
                params.len()
            ));
        }

        let elem_size = self.tensor_elem_size(elem)?;
        let rank = self.tensor_load_rank(t_ptr);
        let count = self.tensor_count_runtime(t_ptr, rank);
        let src_data = self.tensor_data_ptr_dyn(t_ptr, rank, "t.map.sd");
        let (res, res_data) = self.tensor_alloc_runtime(rank, count, elem_size);
        self.tensor_copy_header_dims(t_ptr, res, rank);

        let lhs = ContainerAccess {
            data: src_data,
            len: count,
            elem,
            unsigned,
            bitmap: None,
        };
        let dest = MapDest {
            data: res_data,
            elem,
            bitmap: None,
        };
        // Transcendental-map vectorization (data-spine last leg): a body
        // like `|x| 1.0 / (1.0 + (0.0 - x).exp())` otherwise lowers to a
        // per-element scalar `expf` call that blocks vectorization. When
        // the body is a pure elementwise float expression containing a
        // polynomial transcendental, emit a strip-mined `<W x T>` loop
        // routing exp/ln through the shipped SIMD polynomials instead.
        if self.try_emit_vectorized_map(&lhs, None, params, body, &dest)? {
            return Ok(res.into());
        }
        self.emit_elementwise_map(
            &lhs,
            &MapOther::Unary,
            &MapKernelOp::Closure {
                params,
                body: body.as_ref(),
            },
            &dest,
        )?;
        Ok(res.into())
    }

    /// `zip_with(other, |a, b| body) -> Tensor` — element-wise combine of two
    /// same-shape tensors (in C order) through the inline closure. The shapes
    /// must match exactly (a runtime shape-equality guard, unless the parser
    /// proved them statically equal — but for a method arg we don't have that
    /// side-table, so always guard). Mirrors `compile_column_zip_with` minus
    /// the validity bitmap (a tensor has no null concept). Only the
    /// inline-literal closure form reaches here.
    fn compile_tensor_zip_with(
        &mut self,
        elem: BasicTypeEnum<'ctx>,
        unsigned: bool,
        t_ptr: PointerValue<'ctx>,
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        if args.len() != 2 {
            return Err(format!(
                "Tensor.zip_with expects 2 arguments (other, closure), got {}",
                args.len()
            ));
        }
        let ExprKind::Closure { params, body, .. } = &args[1].value.kind else {
            return Err(
                "Tensor.zip_with expects an inline closure literal under `karac build`; a \
                 closure-valued local / named fn is not yet supported by the native \
                 backend (it works under `karac run`)."
                    .to_string(),
            );
        };
        if params.len() != 2 {
            return Err(format!(
                "Tensor.zip_with closure must take exactly 2 parameters (a, b), got {}",
                params.len()
            ));
        }

        // The second operand — another tensor (identifier or expression).
        let other_ptr = self.compile_expr(&args[0].value)?.into_pointer_value();
        self.emit_tensor_shape_eq_guard(t_ptr, other_ptr)?;

        let elem_size = self.tensor_elem_size(elem)?;
        let rank = self.tensor_load_rank(t_ptr);
        let count = self.tensor_count_runtime(t_ptr, rank);
        let l_data = self.tensor_data_ptr_dyn(t_ptr, rank, "t.zip.ld");
        let r_data = self.tensor_data_ptr_dyn(other_ptr, rank, "t.zip.rd");
        let (res, res_data) = self.tensor_alloc_runtime(rank, count, elem_size);
        self.tensor_copy_header_dims(t_ptr, res, rank);

        let lhs = ContainerAccess {
            data: l_data,
            len: count,
            elem,
            unsigned,
            bitmap: None,
        };
        let r_access = ContainerAccess {
            data: r_data,
            len: count,
            elem,
            unsigned,
            bitmap: None,
        };
        let dest = MapDest {
            data: res_data,
            elem,
            bitmap: None,
        };
        // Transcendental-map vectorization for the two-operand form (same
        // gate as `map`): only when the body is a pure elementwise float
        // expression carrying a polynomial transcendental.
        if self.try_emit_vectorized_map(&lhs, Some(&r_access), params, body, &dest)? {
            return Ok(res.into());
        }
        let other = MapOther::Access(r_access);
        self.emit_elementwise_map(
            &lhs,
            &other,
            &MapKernelOp::Closure {
                params,
                body: body.as_ref(),
            },
            &dest,
        )?;
        Ok(res.into())
    }

    /// Peel a trivial single-final-expression `{ ... }` wrapper off a
    /// closure body — `|x| expr` and `|x| { expr }` must lift identically.
    fn peel_map_block(e: &Expr) -> &Expr {
        match &e.kind {
            ExprKind::Block(b) if b.stmts.is_empty() => match &b.final_expr {
                Some(fe) => Self::peel_map_block(fe),
                None => e,
            },
            _ => e,
        }
    }

    /// Simple binding names of a closure's params, or `None` if any param
    /// isn't a bare binding (the vectorizer needs named lanes).
    fn closure_param_names(params: &[crate::ast::ClosureParam]) -> Option<Vec<String>> {
        params
            .iter()
            .map(|p| match &p.pattern.kind {
                PatternKind::Binding(n) => Some(n.clone()),
                _ => None,
            })
            .collect()
    }

    /// If `callee` is a desugared float operator path `f32::<op>` /
    /// `f64::<op>` (arithmetic lowers to these `Call` nodes before
    /// codegen — `a / b` → `Call(Path(f32::div), [a, b])`), return the op
    /// name; else `None`. The float-type prefix guards against matching an
    /// unrelated `T::add`.
    fn float_path_op(callee: &Expr) -> Option<&str> {
        let ExprKind::Path { segments, .. } = &callee.kind else {
            return None;
        };
        if segments.len() < 2 || !matches!(segments[segments.len() - 2].as_str(), "f32" | "f64") {
            return None;
        }
        Some(segments.last().unwrap().as_str())
    }

    const MAP_ARITH: [&'static str; 5] = ["add", "sub", "mul", "div", "neg"];
    const MAP_UNARY: [&'static str; 9] = [
        "exp", "ln", "sqrt", "sigmoid", "tanh", "floor", "ceil", "round", "trunc",
    ];
    const MAP_TRANS: [&'static str; 4] = ["exp", "ln", "sigmoid", "tanh"];

    /// Whether a map/zip closure body is a pure elementwise float
    /// expression this vectorizer can lift to `<W x T>`. Sets `has_trans`
    /// if the body contains a POLYNOMIAL transcendental (`exp`/`ln`/
    /// `sigmoid`/`tanh`) — the only case we intercept: pure-arithmetic /
    /// `sqrt` / rounding bodies already vectorize (LLVM auto-vec /
    /// hardware intrinsics) and stay bit-exact, so lifting them would only
    /// widen the divergence surface for no win. Grammar: float/int
    /// literals, identifiers (param or loop-invariant float capture),
    /// desugared arithmetic `Call`s (`f32::add/sub/mul/div/neg`), and the
    /// float-unary transcendental / rounding ops in either the `.exp()`
    /// method form or the `f32::exp(x)` desugared-call form.
    fn map_body_liftable(body: &Expr, has_trans: &mut bool) -> bool {
        match &body.kind {
            ExprKind::Float(_, _) | ExprKind::Integer(_, _) | ExprKind::Identifier(_) => true,
            ExprKind::Unary {
                op: UnaryOp::Neg,
                operand,
            } => Self::map_body_liftable(operand, has_trans),
            ExprKind::Binary { op, left, right } => {
                matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div)
                    && Self::map_body_liftable(left, has_trans)
                    && Self::map_body_liftable(right, has_trans)
            }
            // Desugared operator / free-fn call: `f32::div(a, b)`,
            // `f32::exp(x)`, etc.
            ExprKind::Call { callee, args } => {
                let Some(op) = Self::float_path_op(callee) else {
                    return false;
                };
                if Self::MAP_ARITH.contains(&op) {
                    let arity = if op == "neg" { 1 } else { 2 };
                    return args.len() == arity
                        && args
                            .iter()
                            .all(|a| Self::map_body_liftable(&a.value, has_trans));
                }
                if Self::MAP_UNARY.contains(&op) && args.len() == 1 {
                    if Self::MAP_TRANS.contains(&op) {
                        *has_trans = true;
                    }
                    return Self::map_body_liftable(&args[0].value, has_trans);
                }
                false
            }
            ExprKind::MethodCall {
                object,
                method,
                args,
                ..
            } if args.is_empty() && Self::MAP_UNARY.contains(&method.as_str()) => {
                if Self::MAP_TRANS.contains(&method.as_str()) {
                    *has_trans = true;
                }
                Self::map_body_liftable(object, has_trans)
            }
            _ => false,
        }
    }

    /// Free identifiers in a liftable body that are NOT closure params —
    /// the loop-invariant scalar captures the vectorizer splats per lane.
    fn collect_map_captures(body: &Expr, params: &[String], out: &mut Vec<String>) {
        match &body.kind {
            ExprKind::Identifier(n) => {
                if !params.contains(n) && !out.contains(n) {
                    out.push(n.clone());
                }
            }
            ExprKind::Unary { operand, .. } => Self::collect_map_captures(operand, params, out),
            ExprKind::Binary { left, right, .. } => {
                Self::collect_map_captures(left, params, out);
                Self::collect_map_captures(right, params, out);
            }
            ExprKind::Call { args, .. } => {
                for a in args {
                    Self::collect_map_captures(&a.value, params, out);
                }
            }
            ExprKind::MethodCall { object, .. } => Self::collect_map_captures(object, params, out),
            _ => {}
        }
    }

    /// Splat a runtime scalar float across `width` lanes (insert into every
    /// lane of an undef — LLVM's InstCombine folds the chain to a broadcast
    /// shuffle).
    fn vsplat_scalar(
        &self,
        s: inkwell::values::FloatValue<'ctx>,
        vt: inkwell::types::VectorType<'ctx>,
        width: u32,
    ) -> inkwell::values::VectorValue<'ctx> {
        let i32_t = self.context.i32_type();
        let mut v = vt.get_undef();
        for i in 0..width {
            v = self
                .builder
                .build_insert_element(v, s, i32_t.const_int(i as u64, false), "vm.splat")
                .unwrap();
        }
        v
    }

    /// Recursively compile a liftable body at vector `width` — a vector
    /// twin of scalar body evaluation, routing float-unary calls through
    /// `apply_vector_float_unary` (so `exp`/`ln` hit the shipped SIMD
    /// polynomials). `lanes` maps each param / capture name to its
    /// already-splatted `<width x T>` value.
    fn compile_vlift(
        &self,
        expr: &Expr,
        width: u32,
        ft: inkwell::types::FloatType<'ctx>,
        lanes: &std::collections::HashMap<String, inkwell::values::VectorValue<'ctx>>,
    ) -> Result<inkwell::values::VectorValue<'ctx>, String> {
        let vt = ft.vec_type(width);
        let const_splat = |c: f64| -> inkwell::values::VectorValue<'ctx> {
            self.vsplat_scalar(ft.const_float(c), vt, width)
        };
        match &expr.kind {
            ExprKind::Float(v, _) => Ok(const_splat(*v)),
            ExprKind::Integer(v, _) => Ok(const_splat(*v as f64)),
            ExprKind::Identifier(n) => lanes
                .get(n)
                .copied()
                .ok_or_else(|| format!("vlift: unbound identifier '{n}'")),
            ExprKind::Unary {
                op: UnaryOp::Neg,
                operand,
            } => {
                let v = self.compile_vlift(operand, width, ft, lanes)?;
                Ok(self.builder.build_float_neg(v, "vl.neg").unwrap())
            }
            ExprKind::Binary { op, left, right } => {
                let l = self.compile_vlift(left, width, ft, lanes)?;
                let r = self.compile_vlift(right, width, ft, lanes)?;
                Ok(match op {
                    BinOp::Add => self.builder.build_float_add(l, r, "vl.add").unwrap(),
                    BinOp::Sub => self.builder.build_float_sub(l, r, "vl.sub").unwrap(),
                    BinOp::Mul => self.builder.build_float_mul(l, r, "vl.mul").unwrap(),
                    BinOp::Div => self.builder.build_float_div(l, r, "vl.div").unwrap(),
                    _ => return Err("vlift: unsupported binop".to_string()),
                })
            }
            // Desugared operator / transcendental call (`f32::div(a,b)`,
            // `f32::exp(x)`).
            ExprKind::Call { callee, args } => {
                let op = Self::float_path_op(callee)
                    .ok_or_else(|| "vlift: non-float call".to_string())?;
                if Self::MAP_ARITH.contains(&op) {
                    if op == "neg" {
                        let v = self.compile_vlift(&args[0].value, width, ft, lanes)?;
                        return Ok(self.builder.build_float_neg(v, "vl.neg").unwrap());
                    }
                    let l = self.compile_vlift(&args[0].value, width, ft, lanes)?;
                    let r = self.compile_vlift(&args[1].value, width, ft, lanes)?;
                    return Ok(match op {
                        "add" => self.builder.build_float_add(l, r, "vl.add").unwrap(),
                        "sub" => self.builder.build_float_sub(l, r, "vl.sub").unwrap(),
                        "mul" => self.builder.build_float_mul(l, r, "vl.mul").unwrap(),
                        _ => self.builder.build_float_div(l, r, "vl.div").unwrap(),
                    });
                }
                let v = self.compile_vlift(&args[0].value, width, ft, lanes)?;
                self.apply_vector_float_unary(op, v)
                    .ok_or_else(|| format!("vlift: unsupported call op '{op}'"))
            }
            ExprKind::MethodCall { object, method, .. } => {
                let v = self.compile_vlift(object, width, ft, lanes)?;
                self.apply_vector_float_unary(method, v)
                    .ok_or_else(|| format!("vlift: unsupported method '{method}'"))
            }
            _ => Err("vlift: non-liftable node".to_string()),
        }
    }

    /// Emit one strip-mined loop over `[start, end)` at lane `width`,
    /// vector-loading each param source, splatting captures, running the
    /// lifted body, and vector-storing the result. Contiguous C-order
    /// data; the caller sizes `start`/`end` so the strip stays in bounds.
    /// The body always COMPUTES at the vector `width`; `scalar_lanes`
    /// selects how each element is loaded / stored:
    /// * `false` (main loop): vector-load `width` contiguous lanes, step
    ///   `width` — the actual vectorization.
    /// * `true` (remainder): scalar-load ONE element, splat it across the
    ///   `width` lanes, compute, then extract lane 0 and scalar-store,
    ///   step 1. This runs the tail through the IDENTICAL width-`W`
    ///   polynomial (so accuracy matches the main loop exactly) while
    ///   never touching memory past the element — no width-1 vector types,
    ///   no out-of-bounds vector load.
    #[allow(clippy::too_many_arguments)]
    fn emit_vmap_strip(
        &self,
        fn_val: FunctionValue<'ctx>,
        srcs: &[(String, PointerValue<'ctx>)],
        dest_data: PointerValue<'ctx>,
        ft: inkwell::types::FloatType<'ctx>,
        width: u32,
        elem_align: u32,
        start: IntValue<'ctx>,
        end: IntValue<'ctx>,
        caps: &[(String, inkwell::values::FloatValue<'ctx>)],
        body: &Expr,
        scalar_lanes: bool,
    ) -> Result<(), String> {
        let i64_t = self.context.i64_type();
        let i32_t = self.context.i32_type();
        let vt = ft.vec_type(width);
        let step = if scalar_lanes { 1 } else { width };
        let idx = self.builder.build_alloca(i64_t, "vm.i").unwrap();
        self.builder.build_store(idx, start).unwrap();
        let head = self.context.append_basic_block(fn_val, "vm.head");
        let body_bb = self.context.append_basic_block(fn_val, "vm.body");
        let exit = self.context.append_basic_block(fn_val, "vm.exit");
        self.builder.build_unconditional_branch(head).unwrap();

        self.builder.position_at_end(head);
        let iv = self
            .builder
            .build_load(i64_t, idx, "vm.iv")
            .unwrap()
            .into_int_value();
        let more = self
            .builder
            .build_int_compare(IntPredicate::ULT, iv, end, "vm.more")
            .unwrap();
        self.builder
            .build_conditional_branch(more, body_bb, exit)
            .unwrap();

        self.builder.position_at_end(body_bb);
        let mut lanes: std::collections::HashMap<String, inkwell::values::VectorValue<'ctx>> =
            std::collections::HashMap::new();
        for (name, data) in srcs {
            let ptr = unsafe { self.builder.build_gep(ft, *data, &[iv], "vm.sp").unwrap() };
            let lv = if scalar_lanes {
                let s = self
                    .builder
                    .build_load(ft, ptr, "vm.ss")
                    .unwrap()
                    .into_float_value();
                self.vsplat_scalar(s, vt, width)
            } else {
                let ld = self.builder.build_load(vt, ptr, "vm.sv").unwrap();
                let lv = ld.into_vector_value();
                lv.as_instruction()
                    .unwrap()
                    .set_alignment(elem_align)
                    .unwrap();
                lv
            };
            lanes.insert(name.clone(), lv);
        }
        for (name, scalar) in caps {
            lanes.insert(name.clone(), self.vsplat_scalar(*scalar, vt, width));
        }
        let result = self.compile_vlift(body, width, ft, &lanes)?;
        let dp = unsafe {
            self.builder
                .build_gep(ft, dest_data, &[iv], "vm.dp")
                .unwrap()
        };
        if scalar_lanes {
            let lane0 = self
                .builder
                .build_extract_element(result, i32_t.const_zero(), "vm.l0")
                .unwrap();
            self.builder.build_store(dp, lane0).unwrap();
        } else {
            let st = self.builder.build_store(dp, result).unwrap();
            st.set_alignment(elem_align).unwrap();
        }
        let i2 = self
            .builder
            .build_int_add(iv, i64_t.const_int(step as u64, false), "vm.i2")
            .unwrap();
        self.builder.build_store(idx, i2).unwrap();
        self.builder.build_unconditional_branch(head).unwrap();

        self.builder.position_at_end(exit);
        Ok(())
    }

    /// Try to emit a transcendental-map as a strip-mined vector loop (the
    /// data-spine last leg — a scalar `expf`/`logf` call in the map body
    /// otherwise blocks vectorization). `Ok(true)` = emitted; `Ok(false)`
    /// = not eligible, caller uses the scalar `emit_elementwise_map`.
    ///
    /// ACCURACY: the vector path routes `exp`/`ln` through the shipped
    /// `compile_vector_exp`/`_ln` polynomials (both the width-`W` main loop
    /// AND the width-1 tail, so every element uses the SAME approximation —
    /// no mixed-accuracy within one op). This moves a transcendental
    /// tensor-`map` from f32 libm to the ~1-ULP polynomial — the exact,
    /// already-documented `v.exp()` / SIMD-transcendental divergence class
    /// (`karac run` keeps f64 libm). Non-transcendental bodies never reach
    /// here (the `has_trans` gate), so bit-exact paths are untouched.
    fn try_emit_vectorized_map(
        &mut self,
        lhs: &ContainerAccess<'ctx>,
        other: Option<&ContainerAccess<'ctx>>,
        params: &[crate::ast::ClosureParam],
        body: &Expr,
        dest: &MapDest<'ctx>,
    ) -> Result<bool, String> {
        let BasicTypeEnum::FloatType(ft) = lhs.elem else {
            return Ok(false);
        };
        if lhs.bitmap.is_some()
            || dest.bitmap.is_some()
            || other.is_some_and(|o| o.bitmap.is_some())
        {
            return Ok(false);
        }
        let Some(pnames) = Self::closure_param_names(params) else {
            return Ok(false);
        };
        // param count must match the operand count (1 for map, 2 for zip).
        let want = if other.is_some() { 2 } else { 1 };
        if pnames.len() != want {
            return Ok(false);
        }
        let body = Self::peel_map_block(body);
        let mut has_trans = false;
        if !Self::map_body_liftable(body, &mut has_trans) || !has_trans {
            return Ok(false);
        }
        // Loop-invariant captures: resolve each to a scalar float of the
        // element type NOW (before the loops). A non-float capture (e.g. a
        // captured tensor) can't be lane-splatted here — fall back.
        let mut caps: Vec<String> = Vec::new();
        Self::collect_map_captures(body, &pnames, &mut caps);
        let mut cap_scalars: Vec<(String, inkwell::values::FloatValue<'ctx>)> = Vec::new();
        for c in &caps {
            // Resolve captures straight from the variable table — NOT via a
            // synthesized identifier `Expr`, whose fabricated span would
            // miss the span-keyed type side-tables `compile_expr` consults.
            // A capture that isn't a local scalar of the element float type
            // (a global, a captured tensor, a non-float) → fall back.
            let Some(slot) = self.variables.get(c).copied() else {
                return Ok(false);
            };
            let BasicTypeEnum::FloatType(_) = slot.ty else {
                return Ok(false);
            };
            let loaded = self
                .builder
                .build_load(slot.ty, slot.ptr, "vm.cap")
                .unwrap()
                .into_float_value();
            // A capture of the OTHER float width (e.g. a bare `2.5` literal
            // defaults to f64 in an f32 map) casts to the element type —
            // matching the scalar path's implicit promotion.
            let fv = if slot.ty == ft.into() {
                loaded
            } else {
                self.builder
                    .build_float_cast(loaded, ft, "vm.capcast")
                    .unwrap()
            };
            cap_scalars.push((c.clone(), fv));
        }

        let fn_val = self
            .current_fn
            .ok_or_else(|| "vectorized map outside function".to_string())?;
        let i64_t = self.context.i64_type();
        let f32_t = self.context.f32_type();
        let (width, elem_align): (u32, u32) = if ft == f32_t { (8, 4) } else { (4, 8) };
        let log2w = width.trailing_zeros();

        // main_count = (count >> log2w) << log2w  — the largest multiple of
        // `width` ≤ count. Tail (< width) runs at width 1 through the same
        // body compiler, so accuracy is identical across the whole tensor.
        let shr = self
            .builder
            .build_right_shift(
                lhs.len,
                i64_t.const_int(log2w as u64, false),
                false,
                "vm.shr",
            )
            .unwrap();
        let main_count = self
            .builder
            .build_left_shift(shr, i64_t.const_int(log2w as u64, false), "vm.mc")
            .unwrap();

        let srcs: Vec<(String, PointerValue<'ctx>)> = match other {
            Some(o) => vec![(pnames[0].clone(), lhs.data), (pnames[1].clone(), o.data)],
            None => vec![(pnames[0].clone(), lhs.data)],
        };
        self.emit_vmap_strip(
            fn_val,
            &srcs,
            dest.data,
            ft,
            width,
            elem_align,
            i64_t.const_zero(),
            main_count,
            &cap_scalars,
            body,
            false,
        )?;
        self.emit_vmap_strip(
            fn_val,
            &srcs,
            dest.data,
            ft,
            width,
            elem_align,
            main_count,
            lhs.len,
            &cap_scalars,
            body,
            true,
        )?;
        Ok(true)
    }

    /// Full reduce → scalar. `sum`/`prod` fold via `compile_binop_typed`
    /// (inheriting the int-overflow trap); `min`/`max` seed element 0 and
    /// keep the extreme; `mean` is `sum / count` as `f64`. Empty tensor traps.
    fn compile_tensor_full_reduce(
        &mut self,
        method: &str,
        elem: BasicTypeEnum<'ctx>,
        is_unsigned: bool,
        t_ptr: PointerValue<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let i64_t = self.context.i64_type();
        let rank = self.tensor_load_rank(t_ptr);
        let count = self.tensor_count_runtime(t_ptr, rank);
        let nonempty = self
            .builder
            .build_int_compare(IntPredicate::UGT, count, i64_t.const_zero(), "t.red.ne")
            .unwrap();
        self.emit_tensor_guard(nonempty, "cannot reduce an empty tensor")?;
        let data = self.tensor_data_ptr_dyn(t_ptr, rank, "t.red.data");
        // `sum`/`prod`/`mean` funnel through the shared contiguous-fold emitter
        // (`emit_reduce_fold` divides internally for `mean`); `min`/`max` keep
        // the seed-from-element-0 compare-select loop, unified in a later slice.
        if let Some(op) = match method {
            "sum" => Some(ReduceOp::Sum),
            "prod" => Some(ReduceOp::Prod),
            "mean" => Some(ReduceOp::Mean),
            _ => None,
        } {
            let is_float = elem.is_float_type();
            // Seed: `0` for `sum`/`mean` (additive identity), `1` for `prod`,
            // element-typed.
            let seed: BasicValueEnum<'ctx> = if matches!(op, ReduceOp::Prod) {
                if is_float {
                    elem.into_float_type().const_float(1.0).into()
                } else {
                    elem.into_int_type().const_int(1, false).into()
                }
            } else if is_float {
                elem.into_float_type().const_zero().into()
            } else {
                elem.into_int_type().const_zero().into()
            };
            let access = ContainerAccess {
                data,
                len: count,
                elem,
                unsigned: is_unsigned,
                bitmap: None,
            };
            return self.emit_reduce_fold(&access, op, seed);
        }
        // `min` / `max` / `range` — the empty tensor already trapped above, so
        // the shared seed-from-element-0 compare-select loop is safe.
        let access = ContainerAccess {
            data,
            len: count,
            elem,
            unsigned: is_unsigned,
            bitmap: None,
        };
        // `Reduce[T]::range` default (`max - min`) — the builtin `Tensor`
        // implementor doesn't inherit it via the impl-splice, so emit both
        // min/max reductions off the same access and subtract on the element
        // type (int-checked / fsub via `compile_binop_typed`).
        if method == "range" {
            let mx = self.emit_reduce_minmax(&access, true)?;
            let mn = self.emit_reduce_minmax(&access, false)?;
            return self.compile_binop_typed(&BinOp::Sub, mx, mn, is_unsigned);
        }
        self.emit_reduce_minmax(&access, method == "max")
    }

    /// `argmin() -> Option[i64]` / `argmax() -> Option[i64]` (ElementwiseOrd,
    /// S6c): the flat C-order index of the first minimum / maximum over ALL
    /// elements (a tensor has no null concept), or `None` on an EMPTY tensor
    /// (unlike `min`/`max`, which trap). Reuses the dense
    /// [`emit_reduce_argminmax`](super::Codegen::emit_reduce_argminmax) on the
    /// non-empty arm and wraps the index in `Option` via the shared phi builder.
    fn compile_tensor_argminmax(
        &mut self,
        elem: BasicTypeEnum<'ctx>,
        unsigned: bool,
        t_ptr: PointerValue<'ctx>,
        is_max: bool,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let i64_t = self.context.i64_type();
        let fn_val = self
            .current_fn
            .ok_or_else(|| "Tensor.argmin/argmax outside function".to_string())?;
        let rank = self.tensor_load_rank(t_ptr);
        let count = self.tensor_count_runtime(t_ptr, rank);
        let nonempty = self
            .builder
            .build_int_compare(IntPredicate::UGT, count, i64_t.const_zero(), "t.am.ne")
            .unwrap();
        let some_bb = self.context.append_basic_block(fn_val, "t.am.some");
        let none_bb = self.context.append_basic_block(fn_val, "t.am.none");
        let merge_bb = self.context.append_basic_block(fn_val, "t.am.merge");
        self.builder
            .build_conditional_branch(nonempty, some_bb, none_bb)
            .unwrap();

        self.builder.position_at_end(some_bb);
        let data = self.tensor_data_ptr_dyn(t_ptr, rank, "t.am.data");
        let access = ContainerAccess {
            data,
            len: count,
            elem,
            unsigned,
            bitmap: None,
        };
        let best = self.emit_reduce_argminmax(&access, is_max)?;
        let some_end_bb = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(merge_bb).unwrap();

        self.builder.position_at_end(none_bb);
        self.builder.build_unconditional_branch(merge_bb).unwrap();

        self.builder.position_at_end(merge_bb);
        Ok(self.build_option_some_via_phis(&[best], some_end_bb, none_bb, "t.am"))
    }

    /// `sorted() -> Vec[T]` / `argsort() -> Vec[i64]` (ElementwiseOrd, S6c) over
    /// ALL elements in flat C-order (a tensor has no null concept), empty tensor
    /// → an empty Vec. Elements are widened into the shared 8-byte scratch sort
    /// ([`sort_widen_value`](super::Codegen::sort_widen_value)): i64/f64 pass
    /// through, `i8`/`i16`/`i32` sext, `u8`/`u16`/`u32` zext, `f32` fpext — so
    /// every numeric width matches `karac run` (unblocked by the B-2026-07-03-35
    /// narrow-tensor-storage fix; before it, narrow tensor buffers were garbage
    /// under `build` before the sort even ran, and this path rejected them). The
    /// widened tensor path mirrors `compile_column_sorted`: `sorted` sorts a
    /// widened copy then narrows each key back to `elem`; `argsort` sorts a
    /// `0..n` index buffer keyed into the (widened, for narrow elems) data.
    /// **u64** keys via the unsigned `ugt` scratch compare so values ≥ 2^63
    /// order correctly (B-2026-07-07-2) — see
    /// [`sort_key_is_int`](super::Codegen::sort_key_is_int).
    fn compile_tensor_sort(
        &mut self,
        method: &str,
        elem: BasicTypeEnum<'ctx>,
        unsigned: bool,
        t_ptr: PointerValue<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let is_int = self.sort_key_is_int(elem, unsigned, method, "Tensor")?;
        let rank = self.tensor_load_rank(t_ptr);
        let count = self.tensor_count_runtime(t_ptr, rank);
        let data = self.tensor_data_ptr_dyn(t_ptr, rank, "t.sort.data");
        if method == "argsort" {
            // Wide elems key directly into the live data; narrow / f32 elems key
            // into a widened 8-byte copy freed after the sort.
            if self.sort_elem_is_wide(elem) {
                self.stats_argsort(data, count, is_int, unsigned)
            } else {
                let keys = self.sort_widen_data_buffer(data, count, elem, unsigned);
                let vec = self.stats_argsort(keys, count, is_int, unsigned)?;
                self.builder
                    .build_call(self.free_fn, &[keys.into()], "t.sort.keyfree")
                    .unwrap();
                Ok(vec)
            }
        } else {
            // `sorted`: sort a widened copy of the data, then narrow each key
            // back to `elem` in the result Vec (the wide case is an identity
            // widen + steal — byte-identical to the old `stats_sort`).
            let key_buf = self.sort_widen_data_buffer(data, count, elem, unsigned);
            let sk = if is_int {
                SortKey::IntValue { unsigned }
            } else {
                SortKey::Value
            };
            self.emit_sort_scratch(key_buf, count, &sk);
            Ok(self.sort_build_vec_from_keys(key_buf, count, elem))
        }
    }

    /// `sum_axis(n)` / `mean_axis(n)` → a fresh rank-1-lower tensor (rank-1
    /// receiver → a scalar). The result element type is the receiver's for
    /// `sum_axis`, `f64` for `mean_axis`. Reuses the `iter_axis`
    /// outer/inner/n_axis decomposition (runtime axis OK); the result is
    /// zero-init'd then each source element is added into its dropped-axis
    /// cell, and `mean_axis` divides each cell by `dims[n]`. Empty tensor +
    /// axis bounds trap at runtime.
    fn compile_tensor_axis_reduce(
        &mut self,
        method: &str,
        in_elem: BasicTypeEnum<'ctx>,
        in_unsigned: bool,
        rank: usize,
        t_ptr: PointerValue<'ctx>,
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        if args.len() != 1 {
            return Err(format!(
                "{method} takes exactly 1 argument (the axis), found {}",
                args.len()
            ));
        }
        let fn_val = self.current_fn.unwrap();
        let i64_t = self.context.i64_type();
        let is_mean = method == "mean_axis";
        let res_elem: BasicTypeEnum<'ctx> = if is_mean {
            self.context.f64_type().into()
        } else {
            in_elem
        };
        let res_elem_size = self.tensor_elem_size(res_elem)?;

        let axis = self.compile_expr(&args[0].value)?.into_int_value();
        let rank_const = i64_t.const_int(rank as u64, false);
        let oob = self
            .builder
            .build_int_compare(IntPredicate::UGE, axis, rank_const, "t.axr.oob")
            .unwrap();
        let ok = self.builder.build_not(oob, "t.axr.ok").unwrap();
        self.emit_tensor_guard(ok, "axis reduce axis out of bounds")?;

        let rdims: Vec<IntValue<'ctx>> =
            (0..rank).map(|i| self.tensor_load_dim(t_ptr, i)).collect();
        let src_data = self.tensor_data_ptr(t_ptr, rank, "t.axr.src");
        let mut total = i64_t.const_int(1, false);
        for d in &rdims {
            total = self.builder.build_int_mul(total, *d, "t.axr.tot").unwrap();
        }
        let nonempty = self
            .builder
            .build_int_compare(IntPredicate::UGT, total, i64_t.const_zero(), "t.axr.ne")
            .unwrap();
        self.emit_tensor_guard(nonempty, "cannot reduce an empty tensor")?;

        if rank == 1 {
            // Reduce the single axis to a scalar — a contiguous `sum` fold
            // (seed 0), then divide for `mean_axis`.
            let seed: BasicValueEnum<'ctx> = if in_elem.is_float_type() {
                in_elem.into_float_type().const_zero().into()
            } else {
                in_elem.into_int_type().const_zero().into()
            };
            let access = ContainerAccess {
                data: src_data,
                len: rdims[0],
                elem: in_elem,
                unsigned: in_unsigned,
                bitmap: None,
            };
            let acc = self.emit_reduce_fold(&access, ReduceOp::Sum, seed)?;
            if is_mean {
                let sum_f = self.to_float(acc)?;
                let n_f = self
                    .builder
                    .build_unsigned_int_to_float(rdims[0], self.context.f64_type(), "t.axr.nf")
                    .unwrap();
                return Ok(self
                    .builder
                    .build_float_div(sum_f, n_f, "t.axr.m")
                    .unwrap()
                    .into());
            }
            return Ok(acc);
        }

        // outer (i<axis), n_axis (i==axis), inner (i>axis) via runtime select.
        let mut outer = i64_t.const_int(1, false);
        let mut inner = i64_t.const_int(1, false);
        let mut n_axis = i64_t.const_int(1, false);
        for (i, &d) in rdims.iter().enumerate() {
            let ci = i64_t.const_int(i as u64, false);
            let lt = self
                .builder
                .build_int_compare(IntPredicate::ULT, ci, axis, "t.axr.lt")
                .unwrap();
            let eq = self
                .builder
                .build_int_compare(IntPredicate::EQ, ci, axis, "t.axr.eq")
                .unwrap();
            let gt = self
                .builder
                .build_int_compare(IntPredicate::UGT, ci, axis, "t.axr.gt")
                .unwrap();
            let om = self.builder.build_int_mul(outer, d, "t.axr.om").unwrap();
            outer = self
                .builder
                .build_select(lt, om, outer, "t.axr.o")
                .unwrap()
                .into_int_value();
            n_axis = self
                .builder
                .build_select(eq, d, n_axis, "t.axr.na")
                .unwrap()
                .into_int_value();
            let im = self.builder.build_int_mul(inner, d, "t.axr.im").unwrap();
            inner = self
                .builder
                .build_select(gt, im, inner, "t.axr.i")
                .unwrap()
                .into_int_value();
        }
        let sub_rank = rank - 1;
        let sub_dims: Vec<IntValue<'ctx>> = (0..sub_rank)
            .map(|k| {
                let ck = i64_t.const_int(k as u64, false);
                let lt = self
                    .builder
                    .build_int_compare(IntPredicate::ULT, ck, axis, "t.axr.sdlt")
                    .unwrap();
                self.builder
                    .build_select(lt, rdims[k], rdims[k + 1], "t.axr.sd")
                    .unwrap()
                    .into_int_value()
            })
            .collect();
        let result_size = self
            .builder
            .build_int_mul(outer, inner, "t.axr.rsz")
            .unwrap();

        let sub_rank_val = i64_t.const_int(sub_rank as u64, false);
        let (res, res_data) = self.tensor_alloc_runtime(sub_rank_val, result_size, res_elem_size);
        for (k, dv) in sub_dims.iter().enumerate() {
            let slot = self.tensor_header_slot(res, 1 + k as u64, &format!("t.axr.hd{k}"));
            self.builder.build_store(slot, *dv).unwrap();
        }
        // Zero-init the result data (sum identity for int + float).
        let zbytes = self
            .builder
            .build_int_mul(
                result_size,
                i64_t.const_int(res_elem_size, false),
                "t.axr.zb",
            )
            .unwrap();
        self.builder
            .build_memset(res_data, 8, self.context.i8_type().const_zero(), zbytes)
            .map_err(|e| format!("axis-reduce zero-init failed: {:?}", e))?;

        // Accumulate: for f in 0..total, r = (f/(inner*n_axis))*inner + f%inner.
        let inner_naxis = self
            .builder
            .build_int_mul(inner, n_axis, "t.axr.ina")
            .unwrap();
        let fv = self.create_entry_alloca(fn_val, "t.axr.f", i64_t.into());
        self.builder.build_store(fv, i64_t.const_zero()).unwrap();
        let head = self.context.append_basic_block(fn_val, "t.axr.head");
        let body = self.context.append_basic_block(fn_val, "t.axr.body");
        let exit = self.context.append_basic_block(fn_val, "t.axr.exit");
        self.builder.build_unconditional_branch(head).unwrap();
        self.builder.position_at_end(head);
        let f = self
            .builder
            .build_load(i64_t, fv, "t.axr.fv")
            .unwrap()
            .into_int_value();
        let cont = self
            .builder
            .build_int_compare(IntPredicate::ULT, f, total, "t.axr.cont")
            .unwrap();
        self.builder
            .build_conditional_branch(cont, body, exit)
            .unwrap();
        self.builder.position_at_end(body);
        let inner_idx = self
            .builder
            .build_int_unsigned_rem(f, inner, "t.axr.ii")
            .unwrap();
        let outer_idx = self
            .builder
            .build_int_unsigned_div(f, inner_naxis, "t.axr.oi")
            .unwrap();
        let r = {
            let oi_inner = self
                .builder
                .build_int_mul(outer_idx, inner, "t.axr.oii")
                .unwrap();
            self.builder
                .build_int_add(oi_inner, inner_idx, "t.axr.r")
                .unwrap()
        };
        let src_p = unsafe {
            self.builder
                .build_gep(in_elem, src_data, &[f], "t.axr.sp")
                .unwrap()
        };
        let src_v = self.builder.build_load(in_elem, src_p, "t.axr.sv").unwrap();
        let res_p = unsafe {
            self.builder
                .build_gep(res_elem, res_data, &[r], "t.axr.rp")
                .unwrap()
        };
        let cur = self
            .builder
            .build_load(res_elem, res_p, "t.axr.cur")
            .unwrap();
        // `mean_axis` accumulates in f64 (the result type); `sum_axis` in the
        // element type (with the overflow trap via compile_binop_typed).
        let added = if is_mean {
            let sv_f = self.to_float(src_v)?;
            self.builder
                .build_float_add(cur.into_float_value(), sv_f, "t.axr.add")
                .unwrap()
                .into()
        } else {
            self.compile_binop_typed(&BinOp::Add, cur, src_v, in_unsigned)?
        };
        self.builder.build_store(res_p, added).unwrap();
        let nf = self
            .builder
            .build_int_add(f, i64_t.const_int(1, false), "t.axr.nf")
            .unwrap();
        self.builder.build_store(fv, nf).unwrap();
        self.builder.build_unconditional_branch(head).unwrap();
        self.builder.position_at_end(exit);

        // `mean_axis`: divide each cell by n_axis.
        if is_mean {
            let n_axis_f = self
                .builder
                .build_unsigned_int_to_float(n_axis, self.context.f64_type(), "t.axr.naf")
                .unwrap();
            let dv = self.create_entry_alloca(fn_val, "t.axr.d", i64_t.into());
            self.builder.build_store(dv, i64_t.const_zero()).unwrap();
            let dh = self.context.append_basic_block(fn_val, "t.axr.dhead");
            let db = self.context.append_basic_block(fn_val, "t.axr.dbody");
            let de = self.context.append_basic_block(fn_val, "t.axr.dexit");
            self.builder.build_unconditional_branch(dh).unwrap();
            self.builder.position_at_end(dh);
            let di = self
                .builder
                .build_load(i64_t, dv, "t.axr.div")
                .unwrap()
                .into_int_value();
            let dcont = self
                .builder
                .build_int_compare(IntPredicate::ULT, di, result_size, "t.axr.dcont")
                .unwrap();
            self.builder
                .build_conditional_branch(dcont, db, de)
                .unwrap();
            self.builder.position_at_end(db);
            let cell_p = unsafe {
                self.builder
                    .build_gep(res_elem, res_data, &[di], "t.axr.cp")
                    .unwrap()
            };
            let cell = self
                .builder
                .build_load(res_elem, cell_p, "t.axr.cv")
                .unwrap()
                .into_float_value();
            let m = self
                .builder
                .build_float_div(cell, n_axis_f, "t.axr.dm")
                .unwrap();
            self.builder.build_store(cell_p, m).unwrap();
            let dni = self
                .builder
                .build_int_add(di, i64_t.const_int(1, false), "t.axr.dni")
                .unwrap();
            self.builder.build_store(dv, dni).unwrap();
            self.builder.build_unconditional_branch(dh).unwrap();
            self.builder.position_at_end(de);
        }

        Ok(res.into())
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
    /// Coerce a compiled scalar to a tensor's annotated element type before it
    /// is stored (`Tensor.from` leaves, `Tensor.full` fill). A bare integer
    /// literal compiles to `i64` and a bare float literal to `f64`, so storing
    /// it uncoerced into an `elem`-strided slot writes the wrong byte width —
    /// B-2026-07-03-35 (narrow int / `f32` reads back garbage, an f64-bit
    /// integer reads back reinterpreted). Handles the int→float (`sitofp`) case
    /// [`coerce_scalar_to_type`] omits; all same-domain widths (narrow int
    /// trunc/sext, `f32` fptrunc, float widen) go through that shared helper.
    fn coerce_scalar_to_tensor_elem(
        &self,
        val: BasicValueEnum<'ctx>,
        elem: BasicTypeEnum<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        match (val, elem) {
            (BasicValueEnum::IntValue(iv), BasicTypeEnum::FloatType(ft)) => self
                .builder
                .build_signed_int_to_float(iv, ft, "t.elem.i2f")
                .unwrap()
                .into(),
            _ => self.coerce_scalar_to_type(val, elem),
        }
    }

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
        // `get_data_ptr`, not `variables[name].ptr` directly — a ref-param
        // slot holds a pointer to the caller's slot, one deref shy of the
        // control pointer (B-2026-07-02-27, same shape as Column).
        let place = self
            .get_data_ptr(name)
            .ok_or_else(|| format!("Undefined tensor variable '{}'", name))?;
        Ok(self
            .builder
            .build_load(
                self.context.ptr_type(AddressSpace::default()),
                place,
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
