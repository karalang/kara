//! AST-only codegen helpers (no LLVM dependency).
//!
//! Pure functions that inspect / transform AST / prelude types
//! without touching any `inkwell` types. Lives here rather than in
//! `super` so the LLVM-containment invariant (see CLAUDE.md) is
//! visibly enforced at the file boundary — adding an `inkwell::`
//! import to this file should never make sense.

use crate::ast::*;
use crate::token::IntSuffix;

/// Extract a `ConstValue` from a literal `Expr` for slice-1b call-site
/// const-arg binding. Used by `compile_generic_call` to lift explicit
/// `GenericArg::Const(Integer(4))` style call-site args into the
/// `const_subst` map that drives `mangle_mono_name`. Non-literal
/// const-arg shapes (binary expressions, identifier references) are
/// not yet supported at the codegen call-site surface — slice 3 wires
/// the typechecker's evaluator into call-site solving.
/// Convert an `Expr` parsed in value position back to a `TypeExpr` when
/// the expression actually denotes a type. Codegen-side mirror of the
/// typechecker's `expr_as_type_expr`; used by the layout-query
/// intrinsic intercept in `compile_call`.
pub(super) fn expr_as_type_expr_codegen(expr: &Expr) -> Option<TypeExpr> {
    use crate::ast::PathExpr;
    match &expr.kind {
        ExprKind::Identifier(name) => Some(TypeExpr {
            kind: TypeKind::Path(PathExpr {
                segments: vec![name.clone()],
                generic_args: None,
                span: expr.span.clone(),
            }),
            span: expr.span.clone(),
        }),
        ExprKind::Path {
            segments,
            generic_args,
        } => Some(TypeExpr {
            kind: TypeKind::Path(PathExpr {
                segments: segments.clone(),
                generic_args: generic_args.clone(),
                span: expr.span.clone(),
            }),
            span: expr.span.clone(),
        }),
        _ => None,
    }
}

pub(super) fn const_value_from_literal_expr(expr: &Expr) -> Option<crate::prelude::ConstValue> {
    use crate::prelude::ConstValue;
    match &expr.kind {
        ExprKind::Integer(n, sfx) => match sfx {
            Some(IntSuffix::I8) => Some(ConstValue::I8(*n as i8)),
            Some(IntSuffix::I16) => Some(ConstValue::I16(*n as i16)),
            Some(IntSuffix::I32) => Some(ConstValue::I32(*n as i32)),
            Some(IntSuffix::I64) => Some(ConstValue::I64(*n)),
            Some(IntSuffix::U8) => Some(ConstValue::U8(*n as u8)),
            Some(IntSuffix::U16) => Some(ConstValue::U16(*n as u16)),
            Some(IntSuffix::U32) => Some(ConstValue::U32(*n as u32)),
            Some(IntSuffix::U64) => Some(ConstValue::U64(*n as u64)),
            Some(IntSuffix::I128) | Some(IntSuffix::U128) => None,
            None => Some(ConstValue::I64(*n)),
        },
        ExprKind::Bool(b) => Some(ConstValue::Bool(*b)),
        ExprKind::CharLit(c) => Some(ConstValue::Char(*c)),
        ExprKind::ByteLit(b) => Some(ConstValue::U8(*b)),
        _ => None,
    }
}

/// Extract a non-negative integer from a `ConstValue` and coerce to
/// `u32` (used by codegen Array-size extraction sites). Returns
/// `None` for negative integers, floats, bool / char / enum-variant,
/// or values that exceed `u32::MAX`. Const generics slice 4 — used to
/// recover the runtime array length from a const-param binding when
/// the type expression carries `Array[T, N]` with `N` a const-param.
pub(super) fn const_value_as_u32(cv: &crate::prelude::ConstValue) -> Option<u32> {
    use crate::prelude::ConstValue::*;
    let n: i64 = match cv {
        I8(v) => *v as i64,
        I16(v) => *v as i64,
        I32(v) => *v as i64,
        I64(v) => *v,
        I128(v) => i64::try_from(*v).ok()?,
        U8(v) => *v as i64,
        U16(v) => *v as i64,
        U32(v) => *v as i64,
        U64(v) => i64::try_from(*v).ok()?,
        U128(v) => i64::try_from(*v).ok()?,
        Usize(v) => i64::try_from(*v).ok()?,
        Bool(_) | Char(_) | EnumVariant { .. } | F32(_) | F64(_) => return None,
    };
    if n < 0 {
        return None;
    }
    u32::try_from(n).ok()
}

/// Render a `ConstValue` as a name-mangle token. Integers carry their
/// concrete-width suffix so `make_arr[i64, 4i64]` and `make_arr[i64, 4i32]`
/// produce distinct symbols; bool renders as `true` / `false`; char as
/// its numeric codepoint; enum-variant as `EnumName.VariantName`.
pub(super) fn const_value_to_mangle_str(cv: &crate::prelude::ConstValue) -> String {
    use crate::prelude::ConstValue::*;
    match cv {
        I8(v) => format!("{}i8", v),
        I16(v) => format!("{}i16", v),
        I32(v) => format!("{}i32", v),
        I64(v) => format!("{}i64", v),
        I128(v) => format!("{}i128", v),
        U8(v) => format!("{}u8", v),
        U16(v) => format!("{}u16", v),
        U32(v) => format!("{}u32", v),
        U64(v) => format!("{}u64", v),
        U128(v) => format!("{}u128", v),
        Usize(v) => format!("{}usize", v),
        F32(v) => format!("{}f32", v),
        F64(v) => format!("{}f64", v),
        Bool(b) => b.to_string(),
        Char(c) => format!("c{}", *c as u32),
        EnumVariant {
            enum_name,
            variant_name,
            ..
        } => format!("{}.{}", enum_name, variant_name),
    }
}

/// Pull the element `TypeExpr` out of `Vec[T]` — returns `None` for
/// non-Vec shapes or when generic args aren't a single type.
pub(super) fn vec_inner_type_expr(te: &TypeExpr) -> Option<TypeExpr> {
    if let TypeKind::Path(path) = &te.kind {
        let name = path.segments.first().map(|s| s.as_str());
        // Same `Vec[T]` / `VecDeque[T]` codegen alias as
        // `extract_vec_elem_type`: VecDeque rides on Vec's struct shape.
        if name == Some("Vec") || name == Some("VecDeque") {
            if let Some(args) = &path.generic_args {
                if let Some(GenericArg::Type(elem)) = args.first() {
                    return Some(elem.clone());
                }
            }
        }
    }
    None
}

/// Pull the element `TypeExpr` out of `Slice[T]` or `mut Slice[T]`.
pub(super) fn slice_inner_type_expr(te: &TypeExpr) -> Option<TypeExpr> {
    match &te.kind {
        TypeKind::Path(path) => {
            if path.segments.first().map(|s| s.as_str()) != Some("Slice") {
                return None;
            }
            let args = path.generic_args.as_ref()?;
            if args.len() != 1 {
                return None;
            }
            match &args[0] {
                GenericArg::Type(t) => Some(t.clone()),
                _ => None,
            }
        }
        TypeKind::MutSlice(inner) => Some((**inner).clone()),
        _ => None,
    }
}

/// Pull the element `TypeExpr` out of `Set[T]`.
pub(super) fn set_inner_type_expr(te: &TypeExpr) -> Option<TypeExpr> {
    if let TypeKind::Path(path) = &te.kind {
        if path.segments.first().map(|s| s.as_str()) == Some("Set") {
            if let Some(args) = &path.generic_args {
                if let Some(GenericArg::Type(elem)) = args.first() {
                    return Some(elem.clone());
                }
            }
        }
    }
    None
}

/// Pull the (key, value) `TypeExpr`s out of `Map[K, V]`.
pub(super) fn map_kv_type_exprs(te: &TypeExpr) -> Option<(TypeExpr, TypeExpr)> {
    if let TypeKind::Path(path) = &te.kind {
        if path.segments.first().map(|s| s.as_str()) != Some("Map") {
            return None;
        }
        let args = path.generic_args.as_ref()?;
        if args.len() != 2 {
            return None;
        }
        let k = match &args[0] {
            GenericArg::Type(t) => t.clone(),
            _ => return None,
        };
        let v = match &args[1] {
            GenericArg::Type(t) => t.clone(),
            _ => return None,
        };
        Some((k, v))
    } else {
        None
    }
}

/// Extract the type name from an impl block's target TypeExpr.
/// Returns `None` for non-path targets (slice/array/etc.) since those
/// can't carry user-defined impl methods in v1.
pub(super) fn impl_target_name(target: &TypeExpr) -> Option<String> {
    match &target.kind {
        TypeKind::Path(p) => p.segments.last().cloned(),
        _ => None,
    }
}

/// Recognize the `with_provider[R](provider, closure)` call shape at AST
/// level. Mirror of `Interpreter::match_with_provider` and
/// `provider_escape::match_with_provider`. Returns `(R, provider_expr,
/// closure_expr)` when the callee is `Index(Ident("with_provider") |
/// Path(["with_provider"]), R)` with exactly two unlabeled args, else `None`.
pub(super) fn match_with_provider_call<'e>(
    callee: &'e Expr,
    args: &'e [CallArg],
) -> Option<(String, &'e Expr, &'e Expr)> {
    let ExprKind::Index { object, index } = &callee.kind else {
        return None;
    };
    let is_with_provider = match &object.kind {
        ExprKind::Identifier(n) => n == "with_provider",
        ExprKind::Path { segments, .. } => segments.as_slice() == ["with_provider"],
        _ => false,
    };
    if !is_with_provider {
        return None;
    }
    let resource = match &index.kind {
        ExprKind::Identifier(n) => n.clone(),
        ExprKind::Path { segments, .. } => segments.last().cloned()?,
        _ => return None,
    };
    if args.len() != 2 {
        return None;
    }
    Some((resource, &args[0].value, &args[1].value))
}

/// `true` when the method has no `ref T` / `mut ref T` parameters, so its
/// signature matches what the operator-lowering pass emits — every binop
/// rewrite at `lowering.rs` passes operands by value through
/// `Path(Type, method)(a, b)`. Used to break ties between duplicate impls
/// of the same method (e.g. `impl PartialEq for Point { fn eq(ref self,
/// ref Point) }` and `impl Eq for Point { fn eq(self, Point) }`): the
/// value-form impl is the one whose function signature lines up with
/// what the call site actually passes. `make_impl_method_function`
/// already synthesizes `self` as value-typed regardless of the source
/// `ref self`, so this check focuses on the non-self params.
pub(super) fn method_self_is_value(method: &Function) -> bool {
    !method
        .params
        .iter()
        .any(|p| matches!(&p.ty.kind, TypeKind::Ref(_) | TypeKind::MutRef(_)))
}

/// Build a synthetic `Function` node for an impl-block method so the
/// existing `declare_function` / `compile_function` machinery can emit it
/// as an LLVM function named `Type.method`. If the method has a receiver,
/// prepend a `self` parameter whose type mirrors the source self mode:
/// `self` → `Type` (owned), `ref self` → `ref Type`, `mut ref self` →
/// `mut ref Type`. The existing ref-param plumbing in `compile_function`
/// (`ref_params`, `get_data_ptr`, `load_variable` deref) handles each
/// case from there; ref-self mutations write back to the caller's
/// storage via the pointer-typed self param.
pub(super) fn make_impl_method_function(type_name: &str, method: &Function) -> Function {
    let mut f = method.clone();
    f.name = format!("{}.{}", type_name, method.name);
    if let Some(self_kind) = method.self_param.as_ref() {
        let span = method.span.clone();
        let base = TypeExpr {
            kind: TypeKind::Path(PathExpr {
                segments: vec![type_name.to_string()],
                generic_args: None,
                span: span.clone(),
            }),
            span: span.clone(),
        };
        let ty = match self_kind {
            SelfParam::Owned => base,
            SelfParam::Ref => TypeExpr {
                kind: TypeKind::Ref(Box::new(base)),
                span: span.clone(),
            },
            SelfParam::MutRef => TypeExpr {
                kind: TypeKind::MutRef(Box::new(base)),
                span: span.clone(),
            },
        };
        let self_param = Param {
            span: span.clone(),
            pattern: Pattern {
                kind: PatternKind::Binding("self".to_string()),
                span,
            },
            ty,
            default_value: None,
            doc_comment: None,
        };
        f.params.insert(0, self_param);
    }
    f.self_param = None;
    f
}
