//! Sequence in-place mutation, reordering, and slice-view typechecking.
//!
//! Ninth slice of the `infer_method_call` decomposition
//! ([`docs/spikes/state-decomposition-codegen-methodcall.md`]). Holds the
//! `Vec[T]` methods that reorder or shrink the receiver in place, plus the
//! comparator-taking sorts:
//!
//! - `sort_by` / `sorted_by` / `sort_by_key` and their siblings — the
//!   comparator closure is inferred against the element type;
//! - `retain(pred)` — predicate-driven filtering in place;
//! - `dedup()` — adjacent-duplicate removal;
//! - `split_off(at)` — split into a returned tail `Vec[T]`.
//!
//! It also holds the **slice-view / raw-pointer conversions** —
//! `as_slice` / `as_slice_mut` producing a `Slice[T]` view, and `as_ptr` /
//! `as_mut_ptr` producing the raw `*const T` / `*mut T` into the
//! receiver's buffer. Those sit at a different (earlier) position in the
//! chain, so they are a second entry point (`try_slice_view_method`)
//! rather than more blocks in the first.
//!
//! Like the other built-in sequence surfaces, these exist because `Vec` is
//! a prelude type with no `impl` block to dispatch through.
//!
//! The block order is load-bearing — `infer_method_call` is a
//! first-match-wins chain, so these guards keep the exact relative order
//! they had inline, and the function is called from the same position in
//! that chain.
//!
//! Lives in a sibling `impl<'a> super::TypeChecker<'a>` block.

use crate::ast::*;
use crate::token::Span;

use super::types::{IntSize, Type};
use super::TypeErrorKind;

impl<'a> super::TypeChecker<'a> {
    /// Type a sequence in-place mutation / reordering method.
    ///
    /// Returns `Some(ty)` when this surface claims `method` (including
    /// `Some(Type::Error)` when it claims the name but the call is
    /// ill-formed and a diagnostic has been emitted), and `None` when the
    /// name belongs to some later link in the `infer_method_call` chain.
    pub(super) fn try_sequence_mutation_method(
        &mut self,
        method: &str,
        args: &[CallArg],
        span: &Span,
        obj_ty: &Type,
    ) -> Option<Type> {
        // `Vec[T].sort_by` / `Vec[T].sorted_by` / `Vec[T].sort_by_key` /
        // `Vec[T].sorted_by_key` — closure-shape validation. Vec has no
        // stdlib impl block; without this intercept the call falls through
        // to the silent-no-method path below, leaving the closure arg
        // synth-typed with fresh metavars (no pushdown into params, no
        // check on the body's return type). A wrong-shape closure would
        // typecheck and runtime-panic in the interpreter's closure-honoring
        // sort paths. `sort_by` / `sort_by_key` mutate in place and return
        // Unit; `sorted_by` / `sorted_by_key` return a new Vec. Receiver
        // mutability is enforced at the binding layer (calling `.sort_by`
        // on a non-`mut` binding errors there), so no explicit mutability
        // gate is duplicated here.
        // `Vec[T].retain(pred)` / `VecDeque[T].retain(pred)` — keep each element
        // for which `pred: Fn(T) -> bool` holds; mutates in place, returns Unit.
        // Vec has no stdlib impl, so an unhandled `retain` fell to the silent
        // prelude path that infers the closure arg with an UN-seeded `?T` param
        // — `v.retain(|x| x != 3)` then failed "cannot compare '?T0' and 'i64'"
        // (B-2026-07-15-16). Seed the param via the concrete-return `Fn(T) ->
        // bool` check-mode pushdown, exactly as `Option.filter` / the
        // `.iter().filter(..)` adaptor already do. (Map/Set `retain` take a
        // 2-arg `Fn(K, V) -> bool` — a separate arity, not covered here.)
        if method == "retain" {
            let elem_for_vec: Option<Type> = match obj_ty {
                Type::Named { name, args }
                    if (name == "Vec" || name == "VecDeque") && args.len() == 1 =>
                {
                    Some(args[0].clone())
                }
                Type::Ref(inner) | Type::MutRef(inner) => match inner.as_ref() {
                    Type::Named { name, args }
                        if (name == "Vec" || name == "VecDeque") && args.len() == 1 =>
                    {
                        Some(args[0].clone())
                    }
                    _ => None,
                },
                _ => None,
            };
            if let Some(elem) = elem_for_vec {
                if args.len() != 1 {
                    self.type_error(
                        format!(
                            "Vec.retain() expects 1 argument (predicate closure), found {}",
                            args.len()
                        ),
                        *span,
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                } else {
                    let f_ty = Type::Function {
                        params: vec![elem],
                        return_type: Box::new(Type::Bool),
                    };
                    self.check_expr(&args[0].value, &f_ty);
                }
                return Some(Type::Unit);
            }
        }
        if method == "dedup" {
            let is_vec = match obj_ty {
                Type::Named { name, args } => {
                    (name == "Vec" || name == "VecDeque") && args.len() == 1
                }
                Type::Ref(inner) | Type::MutRef(inner) => matches!(
                    inner.as_ref(),
                    Type::Named { name, args } if (name == "Vec" || name == "VecDeque") && args.len() == 1
                ),
                _ => false,
            };
            if is_vec {
                if !args.is_empty() {
                    self.type_error(
                        format!("Vec.dedup() takes no arguments, found {}", args.len()),
                        *span,
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                }
                return Some(Type::Unit);
            }
        }
        if method == "split_off" {
            let vec_elem: Option<Type> = match obj_ty {
                Type::Named { name, args }
                    if (name == "Vec" || name == "VecDeque") && args.len() == 1 =>
                {
                    Some(args[0].clone())
                }
                Type::Ref(inner) | Type::MutRef(inner) => match inner.as_ref() {
                    Type::Named { name, args }
                        if (name == "Vec" || name == "VecDeque") && args.len() == 1 =>
                    {
                        Some(args[0].clone())
                    }
                    _ => None,
                },
                _ => None,
            };
            if let Some(elem) = vec_elem {
                // `split_off(i: i64) -> Vec[T]` — split at index i; self keeps
                // [0, i), the returned Vec owns [i, len).
                if args.len() != 1 {
                    self.type_error(
                        format!(
                            "Vec.split_off() expects 1 argument (index), found {}",
                            args.len()
                        ),
                        *span,
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                } else {
                    self.check_expr(&args[0].value, &Type::Int(IntSize::I64));
                }
                return Some(Type::Named {
                    name: "Vec".to_string(),
                    args: vec![elem],
                });
            }
        }
        if matches!(
            method,
            "sort_by" | "sorted_by" | "sort_by_key" | "sorted_by_key"
        ) {
            let elem_for_vec: Option<Type> = match obj_ty {
                Type::Named { name, args } if name == "Vec" && args.len() == 1 => {
                    Some(args[0].clone())
                }
                Type::Ref(inner) | Type::MutRef(inner) => match inner.as_ref() {
                    Type::Named { name, args } if name == "Vec" && args.len() == 1 => {
                        Some(args[0].clone())
                    }
                    _ => None,
                },
                _ => None,
            };
            if let Some(elem) = elem_for_vec {
                let is_key = method.ends_with("_key");
                let arg_label = if is_key { "key" } else { "comparator" };
                if args.len() != 1 {
                    self.type_error(
                        format!(
                            "Vec.{}() expects 1 argument ({} closure), found {}",
                            method,
                            arg_label,
                            args.len()
                        ),
                        *span,
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                } else if is_key {
                    self.check_sort_key_closure(&elem, &args[0], method, span);
                } else {
                    self.check_sort_comparator(&elem, &args[0], method, span);
                }
                let mutates_in_place = method == "sort_by" || method == "sort_by_key";
                return Some(if mutates_in_place {
                    Type::Unit
                } else {
                    Type::Named {
                        name: "Vec".to_string(),
                        args: vec![elem],
                    }
                });
            }
        }
        None
    }

    /// Type a slice-view / raw-pointer conversion on a sequence receiver.
    ///
    /// `as_slice` / `as_slice_mut` produce a `Slice[T]` view; `as_ptr` /
    /// `as_mut_ptr` produce the raw `*const T` / `*mut T` into the
    /// receiver's buffer. Returns `None` when the name belongs to some
    /// later link in the `infer_method_call` chain.
    pub(super) fn try_slice_view_method(
        &mut self,
        method: &str,
        args: &[CallArg],
        span: &Span,
        obj_ty: &Type,
    ) -> Option<Type> {
        // Stdlib slice views on sequence types. `.as_slice()` / `.as_slice_mut()`
        // on a `Vec[T]` or `Array[T, N]` (or their ref borrows) produce a
        // `Slice[T]` / `mut Slice[T]` handle, per design.md § Slices.
        if (method == "as_slice" || method == "as_slice_mut") && args.is_empty() {
            let mutable = method == "as_slice_mut";
            let element = match obj_ty {
                Type::Array { element, .. } => Some(*element.clone()),
                Type::Slice { element, .. } => Some(*element.clone()),
                Type::Named { name, args } if name == "Vec" && args.len() == 1 => {
                    Some(args[0].clone())
                }
                Type::Ref(inner) | Type::MutRef(inner) => match inner.as_ref() {
                    Type::Array { element, .. } => Some(*element.clone()),
                    Type::Slice { element, .. } => Some(*element.clone()),
                    Type::Named { name, args } if name == "Vec" && args.len() == 1 => {
                        Some(args[0].clone())
                    }
                    _ => None,
                },
                _ => None,
            };
            if let Some(el) = element {
                return Some(Type::Slice {
                    element: Box::new(el),
                    mutable,
                });
            }
        }

        // `Array[T, N].as_ptr()` / `.as_mut_ptr()` and `Vec[T].as_ptr()` /
        // `.as_mut_ptr()` — raw element-0 pointer producers (the language's
        // FFI handoff; mirrors `CStr.as_ptr`). `as_ptr -> *const T`,
        // `as_mut_ptr -> *mut T`. The codegen handler GEPs element 0 of the
        // array storage / loads the `Vec` header's data field. Without a
        // precise arm here the call falls through to the permissive
        // array/vec-method path and binds `Type::Error`, losing the pointer
        // type for downstream FFI / deref. Handles owned arrays + `Vec[T]`
        // and their `ref` / `mut ref` borrows. The `Vec` arm is what feeds a
        // heap buffer (e.g. a framebuffer) to a `host fn` blit — an
        // `Array[u8, N]` of framebuffer size would overflow the wasm stack.
        if (method == "as_ptr" || method == "as_mut_ptr") && args.is_empty() {
            let vec_elem = |t: &Type| -> Option<Type> {
                match t {
                    Type::Named { name, args } if name == "Vec" && args.len() == 1 => {
                        Some(args[0].clone())
                    }
                    _ => None,
                }
            };
            let elem = match obj_ty {
                Type::Array { element, .. } => Some(*element.clone()),
                Type::Named { .. } => vec_elem(obj_ty),
                Type::Ref(inner) | Type::MutRef(inner) => match inner.as_ref() {
                    Type::Array { element, .. } => Some(*element.clone()),
                    other => vec_elem(other),
                },
                _ => None,
            };
            if let Some(el) = elem {
                return Some(Type::Pointer {
                    is_mut: method == "as_mut_ptr",
                    inner: Box::new(el),
                });
            }
        }

        // Fixed-size `Array[T, N]` read-only method surface for a SCALAR
        // element (B-2026-07-17-19). A fixed array is a structural
        // `Type::Array`, not `Type::Named`, so it otherwise falls through to
        // the silent `Type::Error` catch-all — which typechecked `a.get(0)` /
        // `a.contains(x)` clean and then either ran only under the interpreter
        // (which dispatches a fixed array as a Vec) or BUILD-FAILED under AOT.
        // Model exactly the subset both backends now run (`compile_fixed_array_
        // read` provides the matching codegen): `len`/`is_empty`/`get`/`first`/
        // `last`/`contains`. `iter`/`into_iter`/`as_slice`/`as_ptr` are handled
        // by their own arms (above / the iterator-source arm below), so they are
        // deliberately excluded here to fall through to them; the wider Vec
        // surface (`to_vec`/`slice`/`rev`/iterator adaptors) is NOT modelled and
        // is rejected at the structural fall-through. Non-scalar element arrays
        // (String/Vec/struct elements) need heap/borrow handling the codegen
        // arm does not provide, so they are excluded and stay rejected.
        if matches!(
            method,
            "len" | "is_empty" | "get" | "first" | "last" | "contains" | "is_sorted"
        ) {
            let array_elem = match obj_ty {
                Type::Array { element, .. } => Some((**element).clone()),
                Type::Ref(inner) | Type::MutRef(inner) => match inner.as_ref() {
                    Type::Array { element, .. } => Some((**element).clone()),
                    _ => None,
                },
                _ => None,
            };
            if let Some(elem) = array_elem {
                if matches!(
                    elem,
                    Type::Int(_) | Type::UInt(_) | Type::Float(_) | Type::Bool | Type::Char
                ) {
                    let option_elem = Type::Named {
                        name: "Option".to_string(),
                        args: vec![elem.clone()],
                    };
                    match method {
                        "len" => {
                            if !args.is_empty() {
                                self.type_error(
                                    "Array.len() takes no arguments".to_string(),
                                    *span,
                                    TypeErrorKind::WrongNumberOfArgs,
                                );
                            }
                            return Some(Type::Int(IntSize::I64));
                        }
                        "is_empty" => {
                            if !args.is_empty() {
                                self.type_error(
                                    "Array.is_empty() takes no arguments".to_string(),
                                    *span,
                                    TypeErrorKind::WrongNumberOfArgs,
                                );
                            }
                            return Some(Type::Bool);
                        }
                        "first" => {
                            if !args.is_empty() {
                                self.type_error(
                                    "Array.first() takes no arguments".to_string(),
                                    *span,
                                    TypeErrorKind::WrongNumberOfArgs,
                                );
                            }
                            return Some(option_elem);
                        }
                        "last" => {
                            self.expect_optional_index_arg("Array.last", args, span);
                            return Some(option_elem);
                        }
                        "get" => {
                            for arg in args {
                                let at = self.infer_expr(&arg.value);
                                self.check_assignable(
                                    &Type::Int(IntSize::I64),
                                    &at,
                                    arg.value.span,
                                );
                            }
                            return Some(option_elem);
                        }
                        "contains" => {
                            for arg in args {
                                let at = self.infer_expr(&arg.value);
                                self.check_assignable(&elem, &at, arg.value.span);
                            }
                            return Some(Type::Bool);
                        }
                        // The fixed-array twin of `Vec.is_sorted` /
                        // `Slice.is_sorted` (B-2026-08-21-10). Same
                        // total-order gate, so a `Array[f64, N]` receiver is
                        // rejected exactly as `Vec[f64].sort()` is.
                        "is_sorted" => {
                            self.expect_no_args("Array.is_sorted", args, span);
                            self.require_ord_element(&elem, "Array", "is_sorted", span);
                            return Some(Type::Bool);
                        }
                        _ => unreachable!(),
                    }
                }
            }
        }
        None
    }
}
