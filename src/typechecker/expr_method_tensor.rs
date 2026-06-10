//! `Tensor[T, Shape]` instance-method typechecking — the shape-transform
//! family (`iter_axis` / `reshape` / `permute` / `slice` / `squeeze`,
//! phase-11 numerical stdlib). These methods' result types depend on the
//! receiver's shape and on the *syntactic* form of the arguments (literal
//! vs runtime axis, literal vs expression dims), so they can't be
//! expressed in the baked `runtime/stdlib/tensor.kara` signatures;
//! `infer_method_call` intercepts them before the impl-table search and
//! routes here. `shape()` / `rank()` keep normal impl dispatch.
//!
//! Shared posture across the family:
//! - The receiver's *rank* must be statically known: bare-`S` shape
//!   params and splice-bearing shape literals get a focused error
//!   (rank-polymorphic shape transforms are v1.5 shape arithmetic).
//! - Literal arguments produce exact static dims; runtime-valued
//!   arguments degrade the affected dims to `?` (the partially-dynamic
//!   posture design.md commits to until v1.5).
//! - Every compile-time check has an interpreter twin in
//!   `src/interpreter/method_call_tensor.rs` that re-emits the error at
//!   runtime, because `karac run`'s `run_program` path doesn't gate on
//!   typecheck errors.
//!
//! Lives in a sibling `impl<'a> super::TypeChecker<'a>` block.

use crate::ast::{CallArg, Expr, ExprKind};
use crate::token::Span;

use super::types::{is_integer, is_numeric, type_display, ConstArg, DimArg, FloatSize, Type};
use super::TypeErrorKind;

/// The dims of `shape` when every entry is a concrete literal, else
/// `None` (a `?` / dim-param entry means the total element count isn't
/// statically known).
fn fully_static_dims(shape: &[DimArg]) -> Option<Vec<i64>> {
    shape
        .iter()
        .map(|d| match d {
            DimArg::Const(ConstArg::Literal(v)) => Some(*v),
            _ => None,
        })
        .collect()
}

/// Render a dim list the way shape literals are written — `[2, 3, ?]`.
fn shape_display(shape: &[DimArg]) -> String {
    type_display(&Type::Shape(shape.to_vec()))
}

/// Broadcast two shapes per NumPy rules: align from the right, a size-1 dim
/// expands to the partner, missing leading axes are treated as size 1.
/// Returns the merged dim list, or `Err((output_axis, a, b))` for a *static*
/// incompatibility (both dims concrete literals, unequal, neither 1).
fn broadcast_shapes(a: &[DimArg], b: &[DimArg]) -> Result<Vec<DimArg>, (usize, i64, i64)> {
    let out_rank = a.len().max(b.len());
    let off_a = out_rank - a.len();
    let off_b = out_rank - b.len();
    let one = DimArg::Const(ConstArg::Literal(1));
    let mut out = Vec::with_capacity(out_rank);
    for k in 0..out_rank {
        let da = if k >= off_a { &a[k - off_a] } else { &one };
        let db = if k >= off_b { &b[k - off_b] } else { &one };
        out.push(merge_broadcast_dim(da, db).map_err(|(x, y)| (k, x, y))?);
    }
    Ok(out)
}

/// Merge one aligned dim pair under broadcasting. `Err((a, b))` only for two
/// concrete literals that are unequal and neither is 1 (the static-error
/// case). A concrete `1` broadcasts up to its non-literal partner (so the
/// result is the partner, not `1`); a concrete `>1` against a non-literal
/// wins statically (the partner must be 1 or equal at runtime — codegen
/// guards it); two non-literals survive when equal (same param / `?`), else
/// degrade to `?`.
fn merge_broadcast_dim(l: &DimArg, r: &DimArg) -> Result<DimArg, (i64, i64)> {
    use ConstArg::Literal;
    use DimArg::Const;
    match (l, r) {
        (Const(Literal(a)), Const(Literal(b))) => {
            if a == b || *a == 1 || *b == 1 {
                Ok(Const(Literal((*a).max(*b))))
            } else {
                Err((*a, *b))
            }
        }
        // A concrete `1` broadcasts up to the (non-literal) partner.
        (Const(Literal(1)), other) | (other, Const(Literal(1))) => Ok(other.clone()),
        // A concrete `>1` against a non-literal wins (runtime-guarded).
        (Const(Literal(a)), _) | (_, Const(Literal(a))) => Ok(Const(Literal(*a))),
        // Two non-literals: equal survives, else `?`.
        (l, r) => Ok(if l == r { l.clone() } else { DimArg::Dynamic }),
    }
}

impl<'a> super::TypeChecker<'a> {
    /// Entry point for the tensor shape-method family. `tensor_args` is
    /// the receiver's `[T, Shape]` generic-arg list. Performs the shared
    /// static-rank extraction, then dispatches per method.
    pub(super) fn infer_tensor_shape_method(
        &mut self,
        method: &str,
        tensor_args: &[Type],
        args: &[CallArg],
        span: &Span,
    ) -> Type {
        let (elem_ty, shape) = match tensor_args {
            [elem, Type::Shape(dims)] => (elem.clone(), dims.clone()),
            _ => {
                self.type_error(
                    format!(
                        "{} requires the receiver's rank to be statically known; \
                         inside a shape-generic fn the shape is a bare parameter — call \
                         {} at a concrete-shape call site instead (rank-polymorphic \
                         shape transforms are v1.5 shape arithmetic)",
                        method, method
                    ),
                    span.clone(),
                    TypeErrorKind::TypeMismatch,
                );
                for arg in args {
                    self.infer_expr(&arg.value);
                }
                return Type::Error;
            }
        };
        if shape
            .iter()
            .any(|d| matches!(d, DimArg::Splice(_) | DimArg::SpliceVar(_)))
        {
            self.type_error(
                format!(
                    "{} requires the receiver's rank to be statically known; \
                     this shape carries a `...` splice — call {} at a \
                     concrete-shape call site instead (rank-polymorphic shape \
                     transforms are v1.5 shape arithmetic)",
                    method, method
                ),
                span.clone(),
                TypeErrorKind::TypeMismatch,
            );
            for arg in args {
                self.infer_expr(&arg.value);
            }
            return Type::Error;
        }
        match method {
            "iter_axis" => self.infer_tensor_iter_axis(elem_ty, &shape, args, span),
            "reshape" => self.infer_tensor_reshape(elem_ty, &shape, args, span),
            "permute" => self.infer_tensor_permute(elem_ty, &shape, args, span),
            "slice" => self.infer_tensor_slice(elem_ty, &shape, args, span),
            "squeeze" => self.infer_tensor_squeeze(elem_ty, &shape, args, span),
            _ => unreachable!("infer_tensor_shape_method: unrouted method '{method}'"),
        }
    }

    /// `t.iter_axis(n)` — axis iteration. Yields the sub-tensors
    /// obtained by fixing the index along axis `n` (the axis is
    /// *dropped* — NumPy `take(i, axis=n)` semantics), as a `Vec` of
    /// copies: directly `for`-iterable / indexable / `len()`-able, and
    /// honest about the eager-copy semantics (the interpreter has no
    /// view/stride machinery, and a lazy view type is v1.5+ work).
    ///
    /// Typing rules:
    /// - rank ≥ 2, literal axis: compile-time axis bounds check; result
    ///   is `Vec[Tensor[T, dims-with-slot-n-removed]]` — concrete dims,
    ///   `?` dims, and named dim params all survive positionally.
    /// - rank ≥ 2, runtime axis: which dim is dropped isn't statically
    ///   known, so the item shape is rank−1 all-`?`.
    /// - rank 1: result is `Vec[T]` — the sub-tensors would be rank-0,
    ///   which isn't expressible (`[]` shape literals are rejected), and
    ///   scalars are the natural yield.
    fn infer_tensor_iter_axis(
        &mut self,
        elem_ty: Type,
        shape: &[DimArg],
        args: &[CallArg],
        span: &Span,
    ) -> Type {
        if args.len() != 1 {
            self.type_error(
                format!(
                    "iter_axis takes exactly 1 argument (the axis), found {}",
                    args.len()
                ),
                span.clone(),
                TypeErrorKind::WrongNumberOfArgs,
            );
            for arg in args {
                self.infer_expr(&arg.value);
            }
            return Type::Error;
        }
        let rank = shape.len();
        let literal_axis = match self.check_integer_arg(&args[0].value, "iter_axis axis") {
            IntArg::Literal(i) => {
                if i < 0 || i as usize >= rank {
                    self.type_error(
                        format!("axis {} out of bounds for rank-{} tensor", i, rank),
                        args[0].value.span.clone(),
                        TypeErrorKind::TypeMismatch,
                    );
                    return Type::Error;
                }
                Some(i as usize)
            }
            IntArg::Runtime => None,
            IntArg::Bad => return Type::Error,
        };
        let item_ty = if rank == 1 {
            elem_ty
        } else {
            let item_dims: Vec<DimArg> = match literal_axis {
                Some(n) => shape
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| *i != n)
                    .map(|(_, d)| d.clone())
                    .collect(),
                None => vec![DimArg::Dynamic; rank - 1],
            };
            Type::Named {
                name: "Tensor".to_string(),
                args: vec![elem_ty, Type::Shape(item_dims)],
            }
        };
        Type::Named {
            name: "Vec".to_string(),
            args: vec![item_ty],
        }
    }

    /// Tensor reductions (phase-11 line 47, Slice B).
    ///
    /// - **Full reduce** `sum` / `prod` / `min` / `max` (no args) → the
    ///   element type `T`; `mean` → `f64`.
    /// - **Axis reduce** `sum_axis(n)` / `mean_axis(n)` → a tensor of rank-1
    ///   lower (`mean_axis` element type is `f64`). A literal axis drops slot
    ///   `n` exactly; a runtime axis degrades to rank-1 all-`?`. A rank-1
    ///   receiver reduces to a scalar (rank-0 tensors aren't expressible).
    ///
    /// The element type must be numeric (or a `Numeric`-bounded param), and —
    /// like the rest of the tensor surface — the rank must be statically known
    /// (bare-`S` / splice rejected). Full reduce doesn't *need* the rank, but
    /// codegen can't register a splice-shaped tensor param, so requiring it
    /// keeps `karac run` and `karac build` in agreement.
    pub(super) fn infer_tensor_reduce(
        &mut self,
        method: &str,
        tensor_args: &[Type],
        args: &[CallArg],
        span: &Span,
    ) -> Type {
        let elem_ty = match tensor_args.first() {
            Some(t) => t.clone(),
            None => return Type::Error,
        };
        let is_mean = matches!(method, "mean" | "mean_axis");
        let is_axis = matches!(method, "sum_axis" | "mean_axis");
        // `mean` always yields f64; the others preserve the element type.
        let scalar_result = if is_mean {
            Type::Float(FloatSize::F64)
        } else {
            elem_ty.clone()
        };

        if !is_numeric(&elem_ty) && !self.type_param_has_numeric_bound(&elem_ty) {
            self.type_error(
                format!(
                    "tensor reduction `{}` requires a numeric element type, found '{}'",
                    method,
                    type_display(&elem_ty)
                ),
                span.clone(),
                TypeErrorKind::TypeMismatch,
            );
            for arg in args {
                self.infer_expr(&arg.value);
            }
            return Type::Error;
        }

        // Static rank required (splice-free `Shape`), for full and axis alike.
        let shape = match tensor_args.get(1) {
            Some(Type::Shape(dims))
                if !dims
                    .iter()
                    .any(|d| matches!(d, DimArg::Splice(_) | DimArg::SpliceVar(_))) =>
            {
                dims.clone()
            }
            _ => {
                self.type_error(
                    format!(
                        "{} requires the tensor's rank to be statically known; \
                         a bare-`S` or splice-bearing shape isn't supported here \
                         (rank-polymorphic reduction is v1.5 shape arithmetic)",
                        method
                    ),
                    span.clone(),
                    TypeErrorKind::TypeMismatch,
                );
                for arg in args {
                    self.infer_expr(&arg.value);
                }
                return Type::Error;
            }
        };

        if !is_axis {
            // Full reduce — no arguments.
            if !args.is_empty() {
                self.type_error(
                    format!(
                        "tensor reduction `{}` takes no arguments, found {}",
                        method,
                        args.len()
                    ),
                    span.clone(),
                    TypeErrorKind::WrongNumberOfArgs,
                );
                for arg in args {
                    self.infer_expr(&arg.value);
                }
                return Type::Error;
            }
            return scalar_result;
        }

        // Axis reduce — exactly one (axis) argument.
        if args.len() != 1 {
            self.type_error(
                format!(
                    "{} takes exactly 1 argument (the axis), found {}",
                    method,
                    args.len()
                ),
                span.clone(),
                TypeErrorKind::WrongNumberOfArgs,
            );
            for arg in args {
                self.infer_expr(&arg.value);
            }
            return Type::Error;
        }
        let rank = shape.len();
        let literal_axis = match self.check_integer_arg(&args[0].value, &format!("{} axis", method))
        {
            IntArg::Literal(i) => {
                if i < 0 || i as usize >= rank {
                    self.type_error(
                        format!("axis {} out of bounds for rank-{} tensor", i, rank),
                        args[0].value.span.clone(),
                        TypeErrorKind::TypeMismatch,
                    );
                    return Type::Error;
                }
                Some(i as usize)
            }
            IntArg::Runtime => None,
            IntArg::Bad => return Type::Error,
        };
        // A rank-1 receiver reduces along its only axis to a scalar (rank-0
        // tensors aren't expressible), mirroring `iter_axis`'s rank-1 yield.
        if rank == 1 {
            return scalar_result;
        }
        let item_dims: Vec<DimArg> = match literal_axis {
            Some(n) => shape
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != n)
                .map(|(_, d)| d.clone())
                .collect(),
            None => vec![DimArg::Dynamic; rank - 1],
        };
        let item_elem = if is_mean {
            Type::Float(FloatSize::F64)
        } else {
            elem_ty
        };
        Type::Named {
            name: "Tensor".to_string(),
            args: vec![item_elem, Type::Shape(item_dims)],
        }
    }

    /// `t.broadcast_add(other)` / `broadcast_sub` / `broadcast_mul` /
    /// `broadcast_div` — NumPy-style broadcasting element-wise op. The
    /// argument's shape is broadcast against the receiver's (dims align from
    /// the right; a size-1 dim expands; missing leading axes are size 1), and
    /// the result is a fresh tensor of the broadcast shape. Both operands need
    /// a numeric, matching element type and a statically-known (splice-free)
    /// rank. Static-vs-runtime posture mirrors the element-wise op + cross-arg
    /// asserts: a fully-concrete incompatible dim pair is a compile-time
    /// `E_SHAPE`; a `?` / dim-param dim defers compatibility to a codegen
    /// runtime guard. Lowering twins:
    /// `src/interpreter/method_call_tensor.rs::eval_tensor_broadcast` and
    /// `src/codegen/tensor.rs::compile_tensor_broadcast`.
    pub(super) fn infer_tensor_broadcast(
        &mut self,
        method: &str,
        recv_args: &[Type],
        args: &[CallArg],
        span: &Span,
    ) -> Type {
        let (self_elem, self_shape) = match recv_args {
            [elem, Type::Shape(dims)]
                if !dims
                    .iter()
                    .any(|d| matches!(d, DimArg::Splice(_) | DimArg::SpliceVar(_))) =>
            {
                (elem.clone(), dims.clone())
            }
            _ => {
                self.type_error(
                    format!(
                        "{} requires the receiver's rank to be statically known; \
                         a bare-`S` or splice-bearing shape isn't supported here \
                         (rank-polymorphic broadcasting is v1.5 shape arithmetic)",
                        method
                    ),
                    span.clone(),
                    TypeErrorKind::TypeMismatch,
                );
                for arg in args {
                    self.infer_expr(&arg.value);
                }
                return Type::Error;
            }
        };
        if !is_numeric(&self_elem) && !self.type_param_has_numeric_bound(&self_elem) {
            self.type_error(
                format!(
                    "{} requires a numeric element type, found '{}'",
                    method,
                    type_display(&self_elem)
                ),
                span.clone(),
                TypeErrorKind::TypeMismatch,
            );
            for arg in args {
                self.infer_expr(&arg.value);
            }
            return Type::Error;
        }
        if args.len() != 1 {
            self.type_error(
                format!(
                    "{} takes exactly 1 argument (the tensor to broadcast against), found {}",
                    method,
                    args.len()
                ),
                span.clone(),
                TypeErrorKind::WrongNumberOfArgs,
            );
            for arg in args {
                self.infer_expr(&arg.value);
            }
            return Type::Error;
        }
        let other_ty = self.infer_expr(&args[0].value);
        if other_ty == Type::Error {
            return Type::Error;
        }
        // The argument must itself be a tensor (peel one borrow) with a
        // statically-known rank.
        let other_core = match &other_ty {
            Type::Ref(inner) | Type::MutRef(inner) => inner.as_ref(),
            other => other,
        };
        let other_args = match other_core {
            Type::Named { name, args } if name == "Tensor" && args.len() == 2 => args.clone(),
            _ => {
                self.type_error(
                    format!(
                        "{} expects a tensor argument to broadcast against, found '{}'",
                        method,
                        type_display(&other_ty)
                    ),
                    args[0].value.span.clone(),
                    TypeErrorKind::TypeMismatch,
                );
                return Type::Error;
            }
        };
        let (other_elem, other_shape) = match &other_args[..] {
            [elem, Type::Shape(dims)]
                if !dims
                    .iter()
                    .any(|d| matches!(d, DimArg::Splice(_) | DimArg::SpliceVar(_))) =>
            {
                (elem.clone(), dims.clone())
            }
            _ => {
                self.type_error(
                    format!(
                        "{} requires the argument tensor's rank to be statically known; \
                         a bare-`S` or splice-bearing shape isn't supported here",
                        method
                    ),
                    args[0].value.span.clone(),
                    TypeErrorKind::TypeMismatch,
                );
                return Type::Error;
            }
        };
        if self_elem != other_elem {
            self.type_error(
                format!(
                    "{} operands must share an element type; found '{}' and '{}'",
                    method,
                    type_display(&self_elem),
                    type_display(&other_elem)
                ),
                args[0].value.span.clone(),
                TypeErrorKind::TypeMismatch,
            );
            return Type::Error;
        }
        let merged = match broadcast_shapes(&self_shape, &other_shape) {
            Ok(m) => m,
            Err((axis, a, b)) => {
                self.type_error(
                    format!(
                        "shapes are not broadcast-compatible: '{}' vs '{}' — dim {} has \
                         incompatible sizes {} and {} (broadcasting requires equal dims, \
                         or a size-1 dim that expands)",
                        shape_display(&self_shape),
                        shape_display(&other_shape),
                        axis,
                        a,
                        b
                    ),
                    span.clone(),
                    TypeErrorKind::TypeMismatch,
                );
                return Type::Error;
            }
        };
        Type::Named {
            name: "Tensor".to_string(),
            args: vec![self_elem, Type::Shape(merged)],
        }
    }

    /// `t.reshape([d0, d1, ...])` — same elements, new dims, C-order
    /// preserved. The argument must be an *array literal* (mirroring
    /// `Tensor.from`'s dims-from-syntax posture): the literal's length
    /// is the result's static rank, which a runtime `Vec` can't provide.
    /// Integer-literal entries become concrete static dims; any other
    /// integer-typed entry expression becomes a `?` dim. When the
    /// receiver's shape is fully static *and* every entry is a literal,
    /// the element-count product is checked at compile time; otherwise
    /// the check happens at runtime.
    fn infer_tensor_reshape(
        &mut self,
        elem_ty: Type,
        shape: &[DimArg],
        args: &[CallArg],
        span: &Span,
    ) -> Type {
        if args.len() != 1 {
            self.type_error(
                format!(
                    "reshape takes exactly 1 argument (the new dims), found {}",
                    args.len()
                ),
                span.clone(),
                TypeErrorKind::WrongNumberOfArgs,
            );
            for arg in args {
                self.infer_expr(&arg.value);
            }
            return Type::Error;
        }
        let ExprKind::ArrayLiteral(entries) = &args[0].value.kind else {
            self.infer_expr(&args[0].value);
            self.type_error(
                "reshape requires an array-literal dims argument — the result's \
                 static rank comes from the literal's length (`t.reshape([3, 4])`); \
                 runtime-shaped reshape is v1.5 shape arithmetic"
                    .to_string(),
                args[0].value.span.clone(),
                TypeErrorKind::TypeMismatch,
            );
            return Type::Error;
        };
        if entries.is_empty() {
            self.type_error(
                "reshape to rank 0 — `[]` is not a valid dims list (rank-0 \
                 tensors aren't expressible)"
                    .to_string(),
                args[0].value.span.clone(),
                TypeErrorKind::TypeMismatch,
            );
            return Type::Error;
        }
        let mut new_dims: Vec<DimArg> = Vec::with_capacity(entries.len());
        let mut new_product: Option<i64> = Some(1);
        for entry in entries {
            match self.check_integer_arg(entry, "reshape dims") {
                IntArg::Literal(v) => {
                    if v < 0 {
                        self.type_error(
                            format!("reshape dim must be non-negative, got {}", v),
                            entry.span.clone(),
                            TypeErrorKind::TypeMismatch,
                        );
                        return Type::Error;
                    }
                    new_dims.push(DimArg::Const(ConstArg::Literal(v)));
                    new_product = new_product.map(|p| p * v);
                }
                IntArg::Runtime => {
                    new_dims.push(DimArg::Dynamic);
                    new_product = None;
                }
                IntArg::Bad => return Type::Error,
            }
        }
        if let (Some(old_dims), Some(new_count)) = (fully_static_dims(shape), new_product) {
            let old_count: i64 = old_dims.iter().product();
            if old_count != new_count {
                self.type_error(
                    format!(
                        "reshape from {} ({} element(s)) to {} ({} element(s)) — \
                         element counts must match",
                        shape_display(shape),
                        old_count,
                        shape_display(&new_dims),
                        new_count
                    ),
                    span.clone(),
                    TypeErrorKind::TypeMismatch,
                );
                return Type::Error;
            }
        }
        Type::Named {
            name: "Tensor".to_string(),
            args: vec![elem_ty, Type::Shape(new_dims)],
        }
    }

    /// `t.permute([1, 0, 2])` — reorder the axes; result dim `i` is the
    /// receiver's dim `perm[i]` (so `[1, 0]` is the rank-2 transpose).
    /// The axis list must be an array literal of *integer literals* and
    /// an exact permutation of `0..rank`, all checked at compile time —
    /// a runtime-valued permutation would erase every static dim, and
    /// real permutations are spelled literally; runtime perms are v1.5.
    /// Static dims (including `?` slots and named dim params) move with
    /// their axis.
    fn infer_tensor_permute(
        &mut self,
        elem_ty: Type,
        shape: &[DimArg],
        args: &[CallArg],
        span: &Span,
    ) -> Type {
        if args.len() != 1 {
            self.type_error(
                format!(
                    "permute takes exactly 1 argument (the axis list), found {}",
                    args.len()
                ),
                span.clone(),
                TypeErrorKind::WrongNumberOfArgs,
            );
            for arg in args {
                self.infer_expr(&arg.value);
            }
            return Type::Error;
        }
        let rank = shape.len();
        let ExprKind::ArrayLiteral(entries) = &args[0].value.kind else {
            self.infer_expr(&args[0].value);
            self.type_error(
                "permute requires a literal axis-list argument \
                 (`t.permute([1, 0])`) — runtime-valued permutations are v1.5"
                    .to_string(),
                args[0].value.span.clone(),
                TypeErrorKind::TypeMismatch,
            );
            return Type::Error;
        };
        if entries.len() != rank {
            self.type_error(
                format!(
                    "permute axis list has {} entr{}, expected {} (the receiver's rank)",
                    entries.len(),
                    if entries.len() == 1 { "y" } else { "ies" },
                    rank
                ),
                args[0].value.span.clone(),
                TypeErrorKind::TypeMismatch,
            );
            return Type::Error;
        }
        let mut perm: Vec<usize> = Vec::with_capacity(rank);
        let mut seen = vec![false; rank];
        for entry in entries {
            match self.check_integer_arg(entry, "permute axes") {
                IntArg::Literal(i) => {
                    if i < 0 || i as usize >= rank {
                        self.type_error(
                            format!("axis {} out of bounds for rank-{} tensor", i, rank),
                            entry.span.clone(),
                            TypeErrorKind::TypeMismatch,
                        );
                        return Type::Error;
                    }
                    if seen[i as usize] {
                        self.type_error(
                            format!("permute axis list repeats axis {}", i),
                            entry.span.clone(),
                            TypeErrorKind::TypeMismatch,
                        );
                        return Type::Error;
                    }
                    seen[i as usize] = true;
                    perm.push(i as usize);
                }
                IntArg::Runtime => {
                    self.type_error(
                        "permute axes must be integer literals — runtime-valued \
                         permutations are v1.5"
                            .to_string(),
                        entry.span.clone(),
                        TypeErrorKind::TypeMismatch,
                    );
                    return Type::Error;
                }
                IntArg::Bad => return Type::Error,
            }
        }
        let new_dims: Vec<DimArg> = perm.iter().map(|&p| shape[p].clone()).collect();
        Type::Named {
            name: "Tensor".to_string(),
            args: vec![elem_ty, Type::Shape(new_dims)],
        }
    }

    /// `t.slice(axis, start, end)` — contiguous `[start, end)` range
    /// along one axis, every other axis untouched (a copy; `t[i, :, :]`
    /// indexing-syntax slicing is v1.5). With a literal axis the result
    /// keeps every dim except slot `axis`, which becomes `end - start`
    /// when both bounds are literals and `?` otherwise; bounds are
    /// range-checked at compile time when the dim is concrete and both
    /// bounds are literal. A runtime axis preserves rank with all-`?`
    /// dims. `start == end` (an empty slice) is legal — dim 0.
    fn infer_tensor_slice(
        &mut self,
        elem_ty: Type,
        shape: &[DimArg],
        args: &[CallArg],
        span: &Span,
    ) -> Type {
        if args.len() != 3 {
            self.type_error(
                format!(
                    "slice takes exactly 3 arguments (axis, start, end), found {}",
                    args.len()
                ),
                span.clone(),
                TypeErrorKind::WrongNumberOfArgs,
            );
            for arg in args {
                self.infer_expr(&arg.value);
            }
            return Type::Error;
        }
        let rank = shape.len();
        let axis = self.check_integer_arg(&args[0].value, "slice axis");
        let start = self.check_integer_arg(&args[1].value, "slice start");
        let end = self.check_integer_arg(&args[2].value, "slice end");
        if matches!(axis, IntArg::Bad) || matches!(start, IntArg::Bad) || matches!(end, IntArg::Bad)
        {
            return Type::Error;
        }
        let literal_axis = match axis {
            IntArg::Literal(i) => {
                if i < 0 || i as usize >= rank {
                    self.type_error(
                        format!("axis {} out of bounds for rank-{} tensor", i, rank),
                        args[0].value.span.clone(),
                        TypeErrorKind::TypeMismatch,
                    );
                    return Type::Error;
                }
                Some(i as usize)
            }
            _ => None,
        };
        if let IntArg::Literal(s) = start {
            if s < 0 {
                self.type_error(
                    format!("slice start must be non-negative, got {}", s),
                    args[1].value.span.clone(),
                    TypeErrorKind::TypeMismatch,
                );
                return Type::Error;
            }
        }
        if let (IntArg::Literal(s), IntArg::Literal(e)) = (&start, &end) {
            if e < s {
                self.type_error(
                    format!("slice end {} is before start {}", e, s),
                    args[2].value.span.clone(),
                    TypeErrorKind::TypeMismatch,
                );
                return Type::Error;
            }
        }
        let new_dims: Vec<DimArg> = match literal_axis {
            Some(n) => {
                let sliced_dim = match (&start, &end) {
                    (IntArg::Literal(s), IntArg::Literal(e)) => {
                        if let DimArg::Const(ConstArg::Literal(d)) = &shape[n] {
                            if e > d {
                                self.type_error(
                                    format!(
                                        "slice end {} out of bounds for dim {} (size {})",
                                        e, n, d
                                    ),
                                    args[2].value.span.clone(),
                                    TypeErrorKind::TypeMismatch,
                                );
                                return Type::Error;
                            }
                        }
                        DimArg::Const(ConstArg::Literal(e - s))
                    }
                    _ => DimArg::Dynamic,
                };
                shape
                    .iter()
                    .enumerate()
                    .map(|(i, d)| {
                        if i == n {
                            sliced_dim.clone()
                        } else {
                            d.clone()
                        }
                    })
                    .collect()
            }
            None => vec![DimArg::Dynamic; rank],
        };
        Type::Named {
            name: "Tensor".to_string(),
            args: vec![elem_ty, Type::Shape(new_dims)],
        }
    }

    /// `t.squeeze()` / `t.squeeze(n)` — drop size-1 axes.
    ///
    /// - `squeeze(n)` drops exactly slot `n`, whose size must be 1
    ///   (compile error when the dim is a concrete non-1 literal;
    ///   runtime check when it's `?` or a dim param). Requires rank ≥ 2
    ///   — rank-0 tensors aren't expressible. A runtime-valued `n`
    ///   yields the rank−1 all-`?` shape.
    /// - `squeeze()` drops *all* size-1 axes, which requires a fully
    ///   static shape (a `?` dim's size — and therefore the result rank
    ///   — isn't known at compile time; use `squeeze(n)`). An all-ones
    ///   shape is rejected: the result would be rank-0.
    fn infer_tensor_squeeze(
        &mut self,
        elem_ty: Type,
        shape: &[DimArg],
        args: &[CallArg],
        span: &Span,
    ) -> Type {
        let rank = shape.len();
        match args {
            [] => {
                let Some(dims) = fully_static_dims(shape) else {
                    self.type_error(
                        "squeeze() without an axis requires a fully-static shape — \
                         a `?` dim's size (and therefore the result rank) isn't \
                         known at compile time; use `squeeze(n)` for a specific axis"
                            .to_string(),
                        span.clone(),
                        TypeErrorKind::TypeMismatch,
                    );
                    return Type::Error;
                };
                let kept: Vec<DimArg> = dims
                    .iter()
                    .filter(|&&d| d != 1)
                    .map(|&d| DimArg::Const(ConstArg::Literal(d)))
                    .collect();
                if kept.is_empty() {
                    self.type_error(
                        format!(
                            "squeezing every dim of {} produces a rank-0 tensor, \
                             which isn't expressible — keep at least one dim \
                             (use `squeeze(n)`)",
                            shape_display(shape)
                        ),
                        span.clone(),
                        TypeErrorKind::TypeMismatch,
                    );
                    return Type::Error;
                }
                Type::Named {
                    name: "Tensor".to_string(),
                    args: vec![elem_ty, Type::Shape(kept)],
                }
            }
            [axis_arg] => {
                if rank < 2 {
                    self.type_error(
                        "cannot squeeze a rank-1 tensor — the result would be \
                         rank-0, which isn't expressible"
                            .to_string(),
                        span.clone(),
                        TypeErrorKind::TypeMismatch,
                    );
                    self.infer_expr(&axis_arg.value);
                    return Type::Error;
                }
                let new_dims: Vec<DimArg> = match self
                    .check_integer_arg(&axis_arg.value, "squeeze axis")
                {
                    IntArg::Literal(i) => {
                        if i < 0 || i as usize >= rank {
                            self.type_error(
                                format!("axis {} out of bounds for rank-{} tensor", i, rank),
                                axis_arg.value.span.clone(),
                                TypeErrorKind::TypeMismatch,
                            );
                            return Type::Error;
                        }
                        let n = i as usize;
                        if let DimArg::Const(ConstArg::Literal(d)) = &shape[n] {
                            if *d != 1 {
                                self.type_error(
                                    format!("cannot squeeze axis {}: its size is {}, not 1", n, d),
                                    axis_arg.value.span.clone(),
                                    TypeErrorKind::TypeMismatch,
                                );
                                return Type::Error;
                            }
                        }
                        // A `?` / dim-param slot is checked == 1 at
                        // runtime by the interpreter twin.
                        shape
                            .iter()
                            .enumerate()
                            .filter(|(j, _)| *j != n)
                            .map(|(_, d)| d.clone())
                            .collect()
                    }
                    IntArg::Runtime => vec![DimArg::Dynamic; rank - 1],
                    IntArg::Bad => return Type::Error,
                };
                Type::Named {
                    name: "Tensor".to_string(),
                    args: vec![elem_ty, Type::Shape(new_dims)],
                }
            }
            _ => {
                self.type_error(
                    format!(
                        "squeeze takes 0 or 1 argument(s) (an optional axis), found {}",
                        args.len()
                    ),
                    span.clone(),
                    TypeErrorKind::WrongNumberOfArgs,
                );
                for arg in args {
                    self.infer_expr(&arg.value);
                }
                Type::Error
            }
        }
    }

    /// Infer + integer-check one argument expression of the tensor
    /// shape-method family, classifying it as a compile-time literal or
    /// a runtime value. Emits the type error itself on a non-integer
    /// (so callers just bail on `Bad`). The expression is always run
    /// through `infer_expr` so its `expr_types` entry records.
    fn check_integer_arg(&mut self, expr: &Expr, what: &str) -> IntArg {
        let ty = self.infer_expr(expr);
        if !is_integer(&ty) && ty != Type::Error {
            self.type_error(
                format!("{} must be an integer, found '{}'", what, type_display(&ty)),
                expr.span.clone(),
                TypeErrorKind::TypeMismatch,
            );
            return IntArg::Bad;
        }
        match &expr.kind {
            ExprKind::Integer(i, _) => IntArg::Literal(*i),
            _ => IntArg::Runtime,
        }
    }
}

/// Classification of a shape-method integer argument: a compile-time
/// literal (exact static typing), a runtime integer value (degrades the
/// affected dims to `?`), or ill-typed (error already emitted).
enum IntArg {
    Literal(i64),
    Runtime,
    Bad,
}
