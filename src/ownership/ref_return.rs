//! Source-pinning check for borrow returns (`-> ref T` / `-> mut ref T`).
//!
//! design.md § Feature 4 Part 3: "every `ref` value in a well-typed
//! program has a traceable source (a requirement of source pinning) ...
//! if a `ref` can't be traced to a parameter, that's a source pinning
//! error." A function that returns a borrow of a local / owned value /
//! temporary would hand the caller a reference into storage dropped at
//! function exit — a dangling reference.
//!
//! This is the callee half of the borrow-return feature (B-2026-06-07-5).
//! It is polish over codegen rather than a soundness backstop: codegen
//! only produces a return pointer for a `ref`-param-rooted source, so a
//! dangling source already fails at module verification — this check
//! upgrades that raw LLVM error into a clean, spanned diagnostic.
//!
//! Accepted scope mirrors `compile_ref_return_ptr` exactly (lockstep): a
//! returned borrow is accepted iff it is a `ref` parameter / `ref self` /
//! ref-local identifier, a field reached through one, an `if`/scalar-
//! selector `match` selecting among such borrows, a chained borrow-returning
//! free-fn call, or a *borrowed-struct* construction whose every `ref` field
//! traces to a borrowable source (design.md Feature 4 Part 3). Other
//! valid-per-spec forms (method-call chains, destructuring/guarded `match`
//! arms) are reported as not-yet-supported rather than dangling.

use std::collections::HashSet;

use crate::ast::*;
use crate::resolver::SpanKey;
use crate::typechecker::Type;

use super::{BorrowKind, BorrowReturnShape, OwnershipError, OwnershipErrorKind};

impl<'a> super::OwnershipChecker<'a> {
    /// Verify every borrow returned by `f` (`-> ref T` / `-> mut ref T`)
    /// traces to a `ref` parameter. Emits `E0509` at each offending
    /// return expression. No-op for non-borrow-returning functions.
    pub(crate) fn check_ref_return_source_pinning(&mut self, f: &Function) {
        let Some(ret) = &f.return_type else {
            return;
        };
        let is_ref_ret = matches!(ret.kind, TypeKind::Ref(_) | TypeKind::MutRef(_));
        // A `-> StringSlice` return is a borrow return too: the view points
        // into a `String`'s buffer, so it must trace to a borrowable source or
        // it dangles when that source drops at return (design.md § StringSlice:
        // "follows the same borrow rules as `ref T`"). Without this, a view
        // sliced from an owned local would be a silent use-after-free.
        let is_slice_ret = type_expr_is_string_slice(ret);
        // B-2026-08-31-41 — `-> Slice[T]` / `-> mut Slice[T]`, the same borrow
        // return one tier over. Classified separately from `is_slice_ret`
        // because the SOURCE FORMS differ: a `StringSlice` view comes from
        // `recv.slice(a, b)`, a vec-slice from `recv.as_slice()` /
        // `recv.as_slice_mut()` / a range index `recv[a..b]`.
        let is_vec_slice_ret = type_expr_is_vec_slice(ret);
        // Borrowed-collection return (B-2026-07-11-30): the return type is not
        // itself a `ref` but STORES a `ref` in an element/payload position
        // (`Vec[ref T]`, `Option[ref T]`, a tuple with a `ref` member). design.md
        // Feature 4 Part 3 makes this a borrowed collection whose scope is
        // bounded by every element source, so a local-sourced element escapes
        // exactly as a `-> ref Struct` field does. Traced conservatively below.
        let is_container_ref_ret =
            !is_ref_ret && !is_slice_ret && !is_vec_slice_ret && type_expr_has_ref_element(ret);
        if !is_ref_ret && !is_slice_ret && !is_vec_slice_ret && !is_container_ref_ret {
            // Tier-1: plain `ref T` / `mut ref T` (+ `StringSlice`) returns, plus
            // the borrowed-collection case above. Everything else is not a
            // borrow return.
            return;
        }

        // Valid borrow sources: `ref` parameters, plus ref-locals — a
        // `let x = <call to a ref-returning fn>;` whose result is itself
        // a borrow that traces (transitively) to a `ref` parameter.
        let mut ref_params: HashSet<String> = f
            .params
            .iter()
            .filter(|p| matches!(p.ty.kind, TypeKind::Ref(_) | TypeKind::MutRef(_)))
            .flat_map(|p| p.pattern.binding_names())
            .collect();
        // A `ref self` / `mut ref self` receiver is a borrow source too —
        // it lives in `self_param`, not `params` (method accessors:
        // `fn name(ref self) -> ref String { self.name }`).
        if matches!(f.self_param, Some(SelfParam::Ref) | Some(SelfParam::MutRef)) {
            ref_params.insert("self".to_string());
        }
        // A by-value `StringSlice` parameter is itself a borrow handle (its
        // source is the caller's String), so it is a valid source for a
        // returned view (`fn f(s: StringSlice) -> StringSlice { s }`).
        for p in &f.params {
            if type_expr_is_string_slice(&p.ty) {
                ref_params.extend(p.pattern.binding_names());
            }
        }
        // A by-value `Slice[T]` / `mut Slice[T]` parameter is a borrow HANDLE
        // whose backing storage belongs to the caller, so it outlives this
        // frame and is a valid source for a returned slice
        // (`fn f(s: Slice[i64]) -> Slice[i64] { s }`). Exactly the
        // `StringSlice` rule above, and the reason the legal
        // `fn pick(v: ref Vec[i64]) -> Slice[i64]` family keeps passing:
        // `v` is already a `ref` param.
        for p in &f.params {
            if type_expr_is_vec_slice(&p.ty) {
                ref_params.extend(p.pattern.binding_names());
            }
        }
        let ref_returning_fns = self.ref_returning_fn_names();
        let mut ref_locals: HashSet<String> = HashSet::new();
        collect_ref_locals(&f.body, &ref_returning_fns, &mut ref_locals);
        // v2 core §5.1: `let x = ref <place>` borrows the place, so `x` is a
        // borrow source when the place's root is one (B-2026-10-14-38).
        if super::core_rules() {
            collect_core_ref_locals(&f.body, &ref_params, &mut ref_locals);
        }

        // Borrowed-collection path (B-2026-07-11-30) — separate from the Tier-1
        // ref-return classification, since the returned value is an OWNED
        // container (its element *type* is the borrow, not the container).
        if is_container_ref_ret {
            self.check_borrowed_collection_pinning(f, &ref_params, &ref_locals);
            return;
        }
        // StringSlice-locals: `let w = <borrowable>.slice(a, b)` whose receiver
        // root is itself a borrowable source — `w` then carries that borrow and
        // may be returned (`let w = s.slice(0, n); … w`).
        let mut slice_locals: HashSet<String> = HashSet::new();
        if is_slice_ret {
            collect_string_slice_locals(&f.body, &ref_params, &ref_locals, &mut slice_locals);
        }
        if is_vec_slice_ret {
            collect_vec_slice_locals(&f.body, &ref_params, &ref_locals, &mut slice_locals);
        }

        // Every return site: explicit `return e;` anywhere in the body,
        // plus the body's tail expression.
        let mut returns: Vec<&Expr> = Vec::new();
        collect_return_exprs_in_block(&f.body, &mut returns);
        if let Some(tail) = &f.body.final_expr {
            // A SEMICOLON-LESS `return e` in tail position is ALREADY collected
            // above — `collect_return_exprs_in_block` walks `final_expr` and
            // pushes the INNER expression. Pushing the tail again added the raw
            // `Return` node as a second "return site", and no classifier has an
            // arm for it, so every one answered `UnsupportedForm`:
            // `fn f(x: ref S) -> ref S { return x }` was rejected with "this
            // borrow-return form is not yet supported" while the identical
            // `{ return x; }` and `{ x }` spellings both passed. Pre-existing
            // for `-> ref T` and `-> StringSlice` (B-2026-08-31-45); fixed here
            // because the new `Slice` arm would otherwise inherit it.
            if !matches!(tail.kind, ExprKind::Return(_)) {
                returns.push(tail);
            }
        }

        for e in returns {
            // Chained borrow return (`echo(t)` in return position): a
            // borrow-returning **free-fn** call dispatches to the call
            // classifier, which traces every ref-position argument back to a
            // `ref` param / ref-local. Handled only at the top level — a call
            // nested in an `if`/`match` arm still falls through to
            // `classify_borrow_return` (→ `UnsupportedForm`), keeping 3a in
            // lockstep with codegen (which only lowers a top-level call tail).
            let shape = if is_vec_slice_ret {
                // `-> Slice[T]`: the view must trace to a borrowable source.
                // A slice of an owned local — or of an OWNED parameter, which
                // dies at the same return — dangles.
                classify_vec_slice_return(e, &ref_params, &ref_locals, &slice_locals)
            } else if is_slice_ret {
                // `-> StringSlice`: the view must trace to a borrowable source
                // (`ref` param / `ref self` / a `StringSlice` param / ref-local
                // / slice-local). A view sliced from an owned local dangles.
                classify_string_slice_return(e, &ref_params, &ref_locals, &slice_locals)
            } else if super::core_rules() && self.core_owned_call(e) {
                // B-2026-10-22-29: a call that hands back an owned value is a
                // temporary, dropped at the return (core §5.4, §5.5).
                Some(BorrowReturnShape::DanglingSource)
            } else if matches!(&e.kind, ExprKind::Call { .. }) {
                self.classify_borrow_return_call(e, &ref_params, &ref_locals)
            } else if matches!(&e.kind, ExprKind::StructLiteral { .. }) {
                // Borrowed-struct return (`Parser { source: s, position: 0 }`
                // as `-> ref Parser`): pinned iff every `ref` field's
                // initializer traces to a `ref` param. Top-level only (a
                // struct literal nested in an `if`/`match` arm falls through to
                // `classify_borrow_return`'s conservative `StructLiteral`
                // arm) — in lockstep with codegen, which lowers a top-level
                // borrowed-struct tail/return by value.
                self.classify_borrow_return_struct(e, &ref_params, &ref_locals)
            } else if super::core_rules() {
                self.core_return_shape(e, &ref_params, &ref_locals)
            } else {
                classify_borrow_return(e, &ref_params, &ref_locals)
            };
            let Some(shape) = shape else {
                continue;
            };
            // A form only the legacy backend cannot lower yet is not an
            // error under the core: the strict check does not speak for
            // that backend (B-2026-10-19-37).
            if super::core_rules() && shape == BorrowReturnShape::UnsupportedForm {
                continue;
            }
            let (message, suggestion) = match shape {
                BorrowReturnShape::DanglingSource => (
                    "returned borrow does not originate from a `ref` parameter; its source is \
                     dropped when the function returns, leaving a dangling reference"
                        .to_string(),
                    Some(
                        "a borrow return must trace to a `ref` parameter — e.g. \
                         `fn f(x: ref T) -> ref T { x }` or `fn f(u: ref U) -> ref F { u.field }`. \
                         To return an owned value instead, drop `ref` from the return type."
                            .to_string(),
                    ),
                ),
                BorrowReturnShape::UnsupportedForm => (
                    "this borrow-return form is not yet supported".to_string(),
                    Some(
                        "supported today: returning a `ref` parameter (or `ref self`) directly, a \
                         field reached through one, an `if`/scalar-`match` selecting among such \
                         borrows, a chained borrow-returning free-fn call, or a borrowed-struct \
                         construction whose `ref` fields trace to parameters. Method-call chains \
                         and destructuring-`match` arms are tracked follow-ons (B-2026-06-07-5)."
                            .to_string(),
                    ),
                ),
            };
            self.errors.push(OwnershipError {
                message,
                span: e.span,
                kind: OwnershipErrorKind::BorrowReturnNotSourcePinned { shape },
                suggestion,
                replacement: None,
                consume_span: None,
            });
        }
    }

    /// Source-pinning classification for a borrow-returning **free-function**
    /// call in return position (`echo(t)` — chained borrow returns,
    /// B-2026-06-07-5). The call's result borrows from its `ref`-position
    /// arguments (the callee's own source-pinning guarantees that); so it is
    /// pinned to *this* function's `ref` params iff every `ref`-position
    /// argument is itself a borrowable source. A non-`ref`-returning callee,
    /// or a non-identifier callee, is `UnsupportedForm` (method-call chains
    /// included — kept in lockstep with codegen's free-fn-only
    /// `is_borrow_returning_call_expr`). Returns `None` to accept.
    fn classify_borrow_return_call(
        &self,
        e: &Expr,
        ref_params: &HashSet<String>,
        ref_locals: &HashSet<String>,
    ) -> Option<BorrowReturnShape> {
        let ExprKind::Call { callee, args } = &e.kind else {
            return Some(BorrowReturnShape::UnsupportedForm);
        };
        let ExprKind::Identifier(fname) = &callee.kind else {
            return Some(BorrowReturnShape::UnsupportedForm);
        };
        if !self.ref_returning_fn_names().contains(fname) {
            return Some(BorrowReturnShape::UnsupportedForm);
        }
        // Each ref-position arg must trace to a borrowable source; the worst
        // shape across them dominates (a dangling arg → E0509, an
        // unsupported arg → not-yet-supported). Owned (by-value) args carry
        // no borrow into the result, so they are not checked.
        let mut worst: Option<BorrowReturnShape> = None;
        for (i, arg) in args.iter().enumerate() {
            if self.arg_is_borrow_position(callee, i) {
                worst = combine_borrow_shapes(
                    worst,
                    classify_borrow_return(&arg.value, ref_params, ref_locals),
                );
            }
        }
        worst
    }

    /// Source-pinning classification for a *borrowed-struct* construction
    /// returned as `-> ref Struct` (design.md Feature 4 Part 3): "Returning a
    /// borrowed struct follows the same rule as returning a `ref` value: the
    /// borrowed struct's sources must all be parameters. The compiler traces
    /// each `ref` field to its source parameter automatically." A borrowed
    /// struct is one with at least one `ref` field; the struct is pinned iff
    /// every `ref` field's initializer traces to a borrowable source (worst
    /// shape dominates). Owned-field initializers carry no borrow and are
    /// ignored. An owned struct (no `ref` fields) returned as `ref` is a
    /// dangling borrow of a temporary (`DanglingSource`). A `..spread` base
    /// could carry a borrow from an unknown source, so it is conservatively
    /// `UnsupportedForm`. Returns `None` to accept.
    fn classify_borrow_return_struct(
        &self,
        e: &Expr,
        ref_params: &HashSet<String>,
        ref_locals: &HashSet<String>,
    ) -> Option<BorrowReturnShape> {
        let ExprKind::StructLiteral {
            path,
            fields,
            spread,
            ..
        } = &e.kind
        else {
            return Some(BorrowReturnShape::UnsupportedForm);
        };
        if spread.is_some() {
            return Some(BorrowReturnShape::UnsupportedForm);
        }
        let Some(struct_name) = path.first() else {
            return Some(BorrowReturnShape::UnsupportedForm);
        };
        let Some(def) = self.program.items.iter().find_map(|it| match it {
            Item::StructDef(s) if &s.name == struct_name => Some(s),
            _ => None,
        }) else {
            return Some(BorrowReturnShape::DanglingSource);
        };
        let ref_field_names: HashSet<&str> = def
            .fields
            .iter()
            .filter(|f| matches!(f.ty.kind, TypeKind::Ref(_) | TypeKind::MutRef(_)))
            .map(|f| f.name.as_str())
            .collect();
        // Not a borrowed struct: returning an owned struct value as `ref`
        // would hand back a pointer into a temporary dropped at return.
        if ref_field_names.is_empty() {
            return Some(BorrowReturnShape::DanglingSource);
        }
        let mut worst: Option<BorrowReturnShape> = None;
        for fi in fields {
            if ref_field_names.contains(fi.name.as_str()) {
                worst = combine_borrow_shapes(
                    worst,
                    classify_borrow_return(&fi.value, ref_params, ref_locals),
                );
            }
        }
        worst
    }

    /// Borrowed-collection source pinning (B-2026-07-11-30). A `-> Vec[ref T]` /
    /// `-> Option[ref T]` / `-> Result[ref T, E]` return whose element borrows
    /// function-local storage is a dangling escape exactly as a `-> ref Struct`
    /// field is (design.md Feature 4 Part 3: a container with a `ref` in a
    /// stored position is a borrowed collection bounded by every element
    /// source). CONSERVATIVE — this tightens soundness for the clear local-
    /// escape case only: an element source is rejected iff `classify_borrow_-
    /// return` calls it `DanglingSource` (a local / temporary / literal); a
    /// borrowable source (`ref` param / `ref self` / ref-local) or any
    /// untraceable form is accepted, so no currently-valid program is newly
    /// rejected. Two witness shapes are traced: a returned container identifier
    /// whose `.push(arg)` element sources are classified, and a directly
    /// returned `Some(arg)` / `Ok(arg)` wrapper (the borrow-payload position).
    fn check_borrowed_collection_pinning(
        &mut self,
        f: &Function,
        ref_params: &HashSet<String>,
        ref_locals: &HashSet<String>,
    ) {
        // The element (or payload) type: which parts of a pushed tuple or
        // struct are borrows. `None` when the return type does not say, and
        // then a source is classified whole, as before.
        let elem = f.return_type.as_ref().and_then(first_type_arg);
        let mut returns: Vec<&Expr> = Vec::new();
        collect_return_exprs_in_block(&f.body, &mut returns);
        if let Some(tail) = &f.body.final_expr {
            // A SEMICOLON-LESS `return e` in tail position is ALREADY collected
            // above — `collect_return_exprs_in_block` walks `final_expr` and
            // pushes the INNER expression. Pushing the tail again added the raw
            // `Return` node as a second "return site", and no classifier has an
            // arm for it, so every one answered `UnsupportedForm`:
            // `fn f(x: ref S) -> ref S { return x }` was rejected with "this
            // borrow-return form is not yet supported" while the identical
            // `{ return x; }` and `{ x }` spellings both passed. Pre-existing
            // for `-> ref T` and `-> StringSlice` (B-2026-08-31-45); fixed here
            // because the new `Slice` arm would otherwise inherit it.
            if !matches!(tail.kind, ExprKind::Return(_)) {
                returns.push(tail);
            }
        }
        for e in returns {
            let is_dangling = match &e.kind {
                // Returned container identifier (`… v`): every `v.push(src)` in
                // the body contributes an element source. A local source dangles.
                ExprKind::Identifier(name) => {
                    let mut sources: Vec<&Expr> = Vec::new();
                    collect_container_push_sources(&f.body, name, &mut sources);
                    sources
                        .iter()
                        .any(|src| self.element_source_dangles(src, elem, ref_params, ref_locals))
                }
                // Directly returned wrapper: `Some(src)` (`Option[ref T]`) /
                // `Ok(src)` (`Result[ref T, E]`) — `src` is the borrow payload.
                // `Err`/other callees carry no `ref`-payload here.
                ExprKind::Call { callee, args } => {
                    let is_borrow_wrapper = matches!(
                        &callee.kind,
                        ExprKind::Identifier(n) if n == "Some" || n == "Ok"
                    );
                    is_borrow_wrapper
                        && args.len() == 1
                        && self.element_source_dangles(&args[0].value, elem, ref_params, ref_locals)
                }
                // Any other return form is conservatively accepted.
                _ => false,
            };
            if is_dangling {
                self.errors.push(OwnershipError {
                    message: "returned borrowed collection holds an element that borrows \
                              function-local storage; the element's source is dropped when the \
                              function returns, leaving the collection's borrows dangling"
                        .to_string(),
                    span: e.span,
                    kind: OwnershipErrorKind::BorrowReturnNotSourcePinned {
                        shape: BorrowReturnShape::DanglingSource,
                    },
                    suggestion: Some(
                        "every element of a returned `Vec[ref T]` / `Option[ref T]` must borrow a \
                         `ref` parameter (or a value that outlives the call), not a local — push \
                         borrows of parameters. To return owned elements, drop `ref` from the \
                         element type."
                            .to_string(),
                    ),
                    replacement: None,
                    consume_span: None,
                });
            }
        }
    }

    /// Does an element `src` of a returned borrowed collection, of element
    /// type `ty`, borrow storage that dies at return? Only its `ref` parts
    /// can (B-2026-10-27-1): a tuple literal's are the members at `ref`
    /// positions of `ty`, a borrowed struct literal's are its `ref` fields,
    /// and a part whose type holds no `ref` is an owned value that moves
    /// into the collection. `(c, 1)` in a `Vec[(ref Node, i64)]` is pinned
    /// exactly when `c` is, as a bare `c` in a `Vec[ref Node]` is.
    fn element_source_dangles(
        &self,
        src: &Expr,
        ty: Option<&TypeExpr>,
        ref_params: &HashSet<String>,
        ref_locals: &HashSet<String>,
    ) -> bool {
        let dangles = |e: &Expr| {
            matches!(
                classify_borrow_return(e, ref_params, ref_locals),
                Some(BorrowReturnShape::DanglingSource)
            )
        };
        let Some(ty) = ty else {
            return dangles(src);
        };
        if matches!(ty.kind, TypeKind::Ref(_) | TypeKind::MutRef(_)) {
            return dangles(src);
        }
        if !type_expr_contains_ref(ty) {
            return false;
        }
        match (&src.kind, &ty.kind) {
            (ExprKind::Tuple(parts), TypeKind::Tuple(tys)) if parts.len() == tys.len() => parts
                .iter()
                .zip(tys)
                .any(|(p, t)| self.element_source_dangles(p, Some(t), ref_params, ref_locals)),
            (ExprKind::StructLiteral { .. }, _) => matches!(
                self.classify_borrow_return_struct(src, ref_params, ref_locals),
                Some(BorrowReturnShape::DanglingSource)
            ),
            _ => dangles(src),
        }
    }

    /// Caller-side borrow registration (check 3b). When `value` is a call
    /// to a borrow-returning function, the result borrows from the
    /// arguments at the callee's `ref`-parameter positions (conservative
    /// multi-source overapproximation, design.md § Feature 4 Part 3). Push
    /// a persistent active borrow on each such argument's root binding so a
    /// later move/consume of that source while the borrow is live is
    /// rejected by `check_move_of_borrowed` — closing the use-after-free
    /// hole where `let n = name_of(u); sink(u); use(n)` would dangle.
    ///
    /// Must be invoked from the `let` arm *after* the RHS call has been
    /// walked: call-argument borrows are snapshot-restored when the call
    /// returns, so a borrow pushed here (outside that snapshot) is the one
    /// that persists for the binding's scope and drains at scope exit.
    pub(crate) fn register_ref_return_borrows(&mut self, value: &Expr) {
        match &value.kind {
            ExprKind::Call { callee, args } => {
                let ExprKind::Identifier(fname) = &callee.kind else {
                    return;
                };
                if !self.ref_returning_fn_names().contains(fname) {
                    return;
                }
                for (i, arg) in args.iter().enumerate() {
                    if self.arg_is_borrow_position(callee, i) {
                        if let Some(place) = self.place_expr_root(&arg.value) {
                            self.push_active_borrow(BorrowKind::ImmRef, place, arg.value.span);
                        }
                    }
                }
            }
            // Borrow-returning method call (`let n = u.name()`): the result
            // borrows from the receiver. The method's ref-ness is read from
            // the typechecker (the call result type sits at the receiver-span
            // key). Register a borrow on the receiver's root so moving it
            // while `n` is live is rejected.
            ExprKind::MethodCall { object, .. } => {
                let is_ref = matches!(
                    self.typecheck_result
                        .expr_types
                        .get(&SpanKey::from_span(&value.span)),
                    Some(Type::Ref(_) | Type::MutRef(_))
                );
                if is_ref {
                    if let Some(place) = self.place_expr_root(object) {
                        self.push_active_borrow(BorrowKind::ImmRef, place, value.span);
                    }
                }
            }
            _ => {}
        }
    }

    /// Names of program-level functions whose declared return type is a
    /// borrow. Used to recognise ref-locals (`let x = ref_returning()`).
    /// v2 core §5.3 and §5.4 (B-2026-10-19-37): a returned borrow traced
    /// by its origins. A method's result borrows its receiver only, so a
    /// method call is pinned when its receiver is; `unwrap_or` and `??` may
    /// also yield their default, which a literal makes static data. A block
    /// may have statements and the branches take the union of their arms.
    /// Any other form is classified as the legacy check does.
    fn core_return_shape(
        &self,
        e: &Expr,
        ref_params: &HashSet<String>,
        ref_locals: &HashSet<String>,
    ) -> Option<BorrowReturnShape> {
        if self.core_owned_call(e) {
            return Some(BorrowReturnShape::DanglingSource);
        }
        match &e.kind {
            ExprKind::MethodCall {
                object,
                method,
                args,
                ..
            } => {
                let default = if method == "unwrap_or" && args.len() == 1 {
                    self.core_origin_shape(&args[0].value, ref_params, ref_locals)
                } else {
                    None
                };
                combine_borrow_shapes(
                    default,
                    self.core_origin_shape(object, ref_params, ref_locals),
                )
            }
            ExprKind::NilCoalesce { left, right } => combine_borrow_shapes(
                self.core_origin_shape(right, ref_params, ref_locals),
                self.core_origin_shape(left, ref_params, ref_locals),
            ),
            ExprKind::Block(b) => match &b.final_expr {
                Some(t) => self.core_return_shape(t, ref_params, ref_locals),
                None => Some(BorrowReturnShape::UnsupportedForm),
            },
            ExprKind::If {
                then_block,
                else_branch: Some(else_e),
                ..
            } => combine_borrow_shapes(
                match &then_block.final_expr {
                    Some(t) => self.core_return_shape(t, ref_params, ref_locals),
                    None => Some(BorrowReturnShape::UnsupportedForm),
                },
                self.core_return_shape(else_e, ref_params, ref_locals),
            ),
            _ => classify_borrow_return(e, ref_params, ref_locals),
        }
    }

    /// The shape of one origin of a returned borrow: a literal is static
    /// data and a string literal's text is too, a place traces to its root,
    /// a borrow handed on traces through, and a value that owns itself is a
    /// temporary dropped at the return.
    fn core_origin_shape(
        &self,
        e: &Expr,
        ref_params: &HashSet<String>,
        ref_locals: &HashSet<String>,
    ) -> Option<BorrowReturnShape> {
        if matches!(
            e.kind,
            ExprKind::Integer(..)
                | ExprKind::Float(..)
                | ExprKind::Bool(..)
                | ExprKind::CharLit(..)
                | ExprKind::ByteLit(..)
                | ExprKind::StringLit(..)
        ) {
            return None;
        }
        if place_root(e).is_some() {
            return ref_place_shape(e, ref_params, ref_locals);
        }
        let borrowed = self
            .typecheck_result
            .expr_types
            .get(&SpanKey::from_span(&e.span))
            .is_some_and(type_holds_ref);
        if borrowed {
            return self.core_return_shape(e, ref_params, ref_locals);
        }
        Some(BorrowReturnShape::DanglingSource)
    }

    /// Is `e` a call whose recorded result holds no reference: an owned
    /// value, which is a temporary where a borrow is returned (B-2026-10-22-29)?
    fn core_owned_call(&self, e: &Expr) -> bool {
        matches!(e.kind, ExprKind::Call { .. } | ExprKind::MethodCall { .. })
            && self
                .typecheck_result
                .expr_types
                .get(&SpanKey::from_span(&e.span))
                .is_some_and(|t| !matches!(t, Type::Error) && !type_holds_ref(t))
    }

    fn ref_returning_fn_names(&self) -> HashSet<String> {
        let mut names = HashSet::new();
        for item in &self.program.items {
            if let Item::Function(f) = item {
                if let Some(rt) = &f.return_type {
                    if matches!(rt.kind, TypeKind::Ref(_) | TypeKind::MutRef(_)) {
                        names.insert(f.name.clone());
                    }
                }
            }
        }
        names
    }
}

/// Classify a returned expression as a valid borrow source (`None`) or an
/// offending one. Mirrors `compile_ref_return_ptr`'s accepted shapes.
fn classify_borrow_return(
    e: &Expr,
    ref_params: &HashSet<String>,
    ref_locals: &HashSet<String>,
) -> Option<BorrowReturnShape> {
    match &e.kind {
        ExprKind::Identifier(n) => {
            if ref_params.contains(n) || ref_locals.contains(n) {
                None
            } else {
                Some(BorrowReturnShape::DanglingSource)
            }
        }
        // `ref self` returned directly from a method.
        ExprKind::SelfValue => {
            if ref_params.contains("self") {
                None
            } else {
                Some(BorrowReturnShape::DanglingSource)
            }
        }
        ExprKind::FieldAccess { object, .. } => match &object.kind {
            ExprKind::Identifier(b) if ref_params.contains(b) || ref_locals.contains(b) => None,
            // Field through a `ref self` receiver (`self.name`).
            ExprKind::SelfValue if ref_params.contains("self") => None,
            ExprKind::Identifier(_) | ExprKind::SelfValue => {
                Some(BorrowReturnShape::DanglingSource)
            }
            // v2 core: a field chain (`self.a.b`) traces to its root.
            _ if super::core_rules() => ref_place_shape(e, ref_params, ref_locals),
            // A field reached through a non-identifier (a chained field
            // access, a call, …) — valid per spec but Tier-2/3 codegen.
            _ => Some(BorrowReturnShape::UnsupportedForm),
        },
        // v2 core §5.1 / design.md § Returns: `ref <place>` borrows the place,
        // so it is pinned exactly when the place's root is (B-2026-10-14-38).
        ExprKind::Unary {
            op: UnaryOp::Ref | UnaryOp::MutRef,
            operand,
        } if super::core_rules() => ref_place_shape(operand, ref_params, ref_locals),
        // An element place (`self.cells[i]` as an `index_mut` body's tail)
        // borrows its collection, so it traces the same way
        // (B-2026-10-14-37).
        ExprKind::Index { .. } | ExprKind::TupleIndex { .. } if super::core_rules() => {
            ref_place_shape(e, ref_params, ref_locals)
        }
        // Conditional borrow return (`longer`-style Tier 2): every branch
        // must itself be a borrowable shape. A value `if` needs an `else`.
        ExprKind::If {
            then_block,
            else_branch,
            ..
        } => {
            let Some(else_e) = else_branch.as_deref() else {
                return Some(BorrowReturnShape::UnsupportedForm);
            };
            combine_borrow_shapes(
                classify_borrow_return_block(then_block, ref_params, ref_locals),
                classify_borrow_return(else_e, ref_params, ref_locals),
            )
        }
        ExprKind::Block(b) => classify_borrow_return_block(b, ref_params, ref_locals),
        // Conditional borrow return via `match` (sibling of the `if` arm):
        // every arm body must itself be a borrowable shape, combined across
        // arms. Bounded — in EXACT lockstep with codegen's
        // `compile_ref_return_ptr` `Match` arm (an accepting shape here that
        // codegen can't lower falls through to the value-return miscompile) —
        // to: (1) an *identifier* scrutinee, so the scrutinee's own binding
        // owns its drop and the match adds none (a fresh-temp scrutinee needs
        // drop machinery the simplified lowering lacks → `UnsupportedForm`);
        // and (2) guard-free, *binding-free* arms (`pattern_binding_free`).
        // Payload-binding arms (`Some(x) => x`) are the deferred `Option[ref
        // T]` case; guards and undotted unit-variant arms stay follow-ons.
        ExprKind::Match { scrutinee, arms } => {
            if arms.is_empty()
                || !matches!(scrutinee.kind, ExprKind::Identifier(_))
                || !arms.iter().all(match_arm_borrowable_shape)
            {
                return Some(BorrowReturnShape::UnsupportedForm);
            }
            arms.iter()
                .map(|a| classify_borrow_return(&a.body, ref_params, ref_locals))
                .reduce(combine_borrow_shapes)
                .unwrap_or(Some(BorrowReturnShape::UnsupportedForm))
        }
        // Literals and temporaries are unambiguously dangling; the rest
        // (`Call`/`MethodCall`/…) are valid-but-unsupported.
        ExprKind::Integer(..)
        | ExprKind::Float(..)
        | ExprKind::Bool(..)
        | ExprKind::CharLit(..)
        | ExprKind::StringLit(..)
        | ExprKind::ArrayLiteral(..)
        | ExprKind::StructLiteral { .. }
        | ExprKind::Tuple(..) => Some(BorrowReturnShape::DanglingSource),
        _ => Some(BorrowReturnShape::UnsupportedForm),
    }
}

/// Classify a block's tail as a borrow source. Matches codegen's Tier-2
/// capability: statement-free blocks only (a block with preceding
/// statements is reported as not-yet-supported, never miscompiled).
fn classify_borrow_return_block(
    b: &Block,
    ref_params: &HashSet<String>,
    ref_locals: &HashSet<String>,
) -> Option<BorrowReturnShape> {
    if !b.stmts.is_empty() {
        return Some(BorrowReturnShape::UnsupportedForm);
    }
    match &b.final_expr {
        Some(e) => classify_borrow_return(e, ref_params, ref_locals),
        None => Some(BorrowReturnShape::UnsupportedForm),
    }
}

/// A `match` arm is in-scope for borrow-return classification only when it
/// has no guard and its pattern binds nothing (`pattern_binding_free`). Kept
/// identical to codegen's `expr_ops.rs::ref_return_match_arm_ok` so the
/// source-pinning check and the lowering accept the same set (lockstep — see
/// that fn's note).
fn match_arm_borrowable_shape(arm: &MatchArm) -> bool {
    arm.guard.is_none() && pattern_binding_free(&arm.pattern)
}

/// A pattern that binds no variable. Byte-identical to codegen's
/// `expr_ops.rs::ref_return_pattern_binding_free` — see that fn for the full
/// rationale (the dot in a `Binding` name disambiguates a no-bind dotted unit
/// variant like `Side.Left` from a payload binding, the distinction the
/// ownership pass cannot make via types).
fn pattern_binding_free(pat: &Pattern) -> bool {
    match &pat.kind {
        PatternKind::Wildcard => true,
        PatternKind::Literal(
            LiteralPattern::Integer(..) | LiteralPattern::Char(..) | LiteralPattern::Bool(..),
        ) => true,
        PatternKind::TupleVariant { patterns, .. } => patterns
            .iter()
            .all(|p| matches!(p.kind, PatternKind::Wildcard)),
        PatternKind::Binding(name) => name.contains('.'),
        _ => false,
    }
}

/// Merge two branch classifications. `None` is OK; a genuinely dangling
/// branch dominates (it's a real source-pinning error), otherwise any
/// not-yet-supported branch makes the whole form unsupported.
fn combine_borrow_shapes(
    a: Option<BorrowReturnShape>,
    b: Option<BorrowReturnShape>,
) -> Option<BorrowReturnShape> {
    match (a, b) {
        (None, None) => None,
        (Some(BorrowReturnShape::DanglingSource), _)
        | (_, Some(BorrowReturnShape::DanglingSource)) => Some(BorrowReturnShape::DanglingSource),
        _ => Some(BorrowReturnShape::UnsupportedForm),
    }
}

/// A `StringSlice` type-expr — a `Path` whose head segment is `StringSlice`.
/// Whether `te` is a `Slice[T]` / `mut Slice[T]` return type.
///
/// B-2026-08-31-41 — the sibling of [`type_expr_is_string_slice`], and it is a
/// borrow return for exactly the reason that one is. design.md § Slices,
/// "Borrow rules": "`Slice[T]` follows the same rules as `ref T` — it cannot
/// outlive the collection it borrows from." Without this arm the whole
/// source-pinning check early-returned on any `-> Slice[T]` signature, so
/// `fn f() -> Slice[i64] { let mut v = Vec.new(); …; return v.as_slice() }`
/// passed `karac check` and then read freed memory under `karac build` while
/// the interpreter printed live values.
///
/// `mut Slice[T]` is `TypeKind::MutSlice`, a spelling distinct from the
/// `Path(["Slice"])` one — both reach here, and only covering the path form
/// would leave the `mut` escape unchecked.
fn type_expr_is_vec_slice(te: &TypeExpr) -> bool {
    matches!(&te.kind, TypeKind::MutSlice(_))
        || matches!(
            &te.kind,
            TypeKind::Path(p) if p.segments.first().map(|s| s.as_str()) == Some("Slice")
        )
}

fn type_expr_is_string_slice(te: &TypeExpr) -> bool {
    matches!(
        &te.kind,
        TypeKind::Path(p) if p.segments.first().map(|s| s.as_str()) == Some("StringSlice")
    )
}

/// Source-pinning classification for a `-> StringSlice` return expression. A
/// returned view must trace to a borrowable source — a `ref` parameter /
/// `ref self`, a by-value `StringSlice` parameter, a ref-local, or a
/// slice-local (`let w = s.slice(..)`). A view sliced from an owned local /
/// temporary dangles (`DanglingSource`). Forms beyond the direct
/// `recv.slice(..)` / identifier / simple-`if` shapes are reported
/// `UnsupportedForm` (sound — a clean error, never a miscompile). Returns
/// `None` to accept. Mirrors `classify_borrow_return`'s structure for `ref T`.
fn classify_string_slice_return(
    e: &Expr,
    ref_params: &HashSet<String>,
    ref_locals: &HashSet<String>,
    slice_locals: &HashSet<String>,
) -> Option<BorrowReturnShape> {
    match &e.kind {
        // `recv.slice(a, b)` — the view borrows from `recv`'s root.
        ExprKind::MethodCall { object, method, .. } if method == "slice" => {
            classify_slice_receiver_root(object, ref_params, ref_locals, slice_locals)
        }
        // A `StringSlice` identifier: a by-value StringSlice param (in
        // `ref_params`) or a slice-local is a valid source; an owned local view
        // returned after its source dropped would dangle.
        ExprKind::Identifier(n) => {
            if ref_params.contains(n) || slice_locals.contains(n) {
                None
            } else {
                Some(BorrowReturnShape::DanglingSource)
            }
        }
        ExprKind::SelfValue => {
            if ref_params.contains("self") {
                None
            } else {
                Some(BorrowReturnShape::DanglingSource)
            }
        }
        ExprKind::If {
            then_block,
            else_branch,
            ..
        } => {
            let Some(else_e) = else_branch.as_deref() else {
                return Some(BorrowReturnShape::UnsupportedForm);
            };
            combine_borrow_shapes(
                classify_string_slice_block(then_block, ref_params, ref_locals, slice_locals),
                classify_string_slice_return(else_e, ref_params, ref_locals, slice_locals),
            )
        }
        ExprKind::Block(b) => classify_string_slice_block(b, ref_params, ref_locals, slice_locals),
        // `match`, chained calls returning a view, etc. — sound follow-ons.
        _ => Some(BorrowReturnShape::UnsupportedForm),
    }
}

/// Source-pinning classification for a `-> Slice[T]` / `-> mut Slice[T]`
/// return expression (B-2026-08-31-41). The `StringSlice` twin of
/// [`classify_string_slice_return`], differing only in which expression forms
/// PRODUCE the view: `recv.as_slice()` / `recv.as_slice_mut()` and a range
/// index `recv[a..b]`, where the string view uses `recv.slice(a, b)`.
///
/// A returned slice must trace to a source that outlives the frame — a `ref` /
/// `mut ref` parameter, `ref self`, a by-value `Slice` parameter, a ref-local,
/// or a slice-local bound from one of those. A slice of an owned local or an
/// OWNED PARAMETER dangles: both die at this return, and the compiled program
/// then reads freed memory while the interpreter prints live values.
///
/// Unrecognized forms are `UnsupportedForm`, which is a clean error rather
/// than an unsound accept — the same trade every other classifier here makes.
fn classify_vec_slice_return(
    e: &Expr,
    ref_params: &HashSet<String>,
    ref_locals: &HashSet<String>,
    slice_locals: &HashSet<String>,
) -> Option<BorrowReturnShape> {
    match &e.kind {
        // `recv.as_slice()` / `recv.as_slice_mut()` — borrows from `recv`'s root.
        ExprKind::MethodCall { object, method, .. }
            if method == "as_slice" || method == "as_slice_mut" =>
        {
            classify_vec_slice_root(object, ref_params, ref_locals, slice_locals)
        }
        // `recv[a..b]` — a range index is the other slice-producing form. The
        // index expression itself is irrelevant to provenance; only the
        // receiver's root decides whether the view outlives its storage.
        ExprKind::Index { object, .. } => {
            classify_vec_slice_root(object, ref_params, ref_locals, slice_locals)
        }
        // A slice-typed identifier / `self` / place chain: delegate to the same
        // root walk, so `self.v` and `b.inner.xs` are judged by their ROOT.
        ExprKind::Identifier(_)
        | ExprKind::SelfValue
        | ExprKind::FieldAccess { .. }
        | ExprKind::TupleIndex { .. } => {
            classify_vec_slice_root(e, ref_params, ref_locals, slice_locals)
        }
        ExprKind::If {
            then_block,
            else_branch,
            ..
        } => {
            let Some(else_e) = else_branch.as_deref() else {
                return Some(BorrowReturnShape::UnsupportedForm);
            };
            combine_borrow_shapes(
                classify_vec_slice_block(then_block, ref_params, ref_locals, slice_locals),
                classify_vec_slice_return(else_e, ref_params, ref_locals, slice_locals),
            )
        }
        ExprKind::Block(b) => classify_vec_slice_block(b, ref_params, ref_locals, slice_locals),
        _ => Some(BorrowReturnShape::UnsupportedForm),
    }
}

/// Walk a slice's source expression down to its ROOT binding and judge whether
/// that root outlives the frame.
///
/// The `StringSlice` sibling [`classify_slice_receiver_root`] only accepts a
/// bare identifier or `self`, which is why this is a separate walk rather than
/// a reuse: a slice source is very often a PLACE CHAIN. `fn view(ref self) ->
/// Slice[i64] { return self.v.as_slice(); }` is sound — `ref self` outlives the
/// call, so a slice of `self.v` does too — and judging it by the whole
/// expression instead of its root reported it as an unsupported form.
///
/// Mirrors `borrow.rs`'s `place_expr_root` in which expression shapes count as
/// projections off a root: field access, index, tuple index, and a
/// slice-of-slice through `as_slice`. Anything else is `UnsupportedForm` — a
/// clean error rather than an unsound accept.
fn classify_vec_slice_root(
    e: &Expr,
    ref_params: &HashSet<String>,
    ref_locals: &HashSet<String>,
    slice_locals: &HashSet<String>,
) -> Option<BorrowReturnShape> {
    match &e.kind {
        ExprKind::Identifier(n) => {
            if ref_params.contains(n) || ref_locals.contains(n) || slice_locals.contains(n) {
                None
            } else {
                Some(BorrowReturnShape::DanglingSource)
            }
        }
        ExprKind::SelfValue => {
            if ref_params.contains("self") {
                None
            } else {
                Some(BorrowReturnShape::DanglingSource)
            }
        }
        // Projections off a root — the root decides.
        ExprKind::FieldAccess { object, .. }
        | ExprKind::Index { object, .. }
        | ExprKind::TupleIndex { object, .. } => {
            classify_vec_slice_root(object, ref_params, ref_locals, slice_locals)
        }
        // A slice taken from a slice keeps the original root.
        ExprKind::MethodCall { object, method, .. }
            if method == "as_slice" || method == "as_slice_mut" =>
        {
            classify_vec_slice_root(object, ref_params, ref_locals, slice_locals)
        }
        _ => Some(BorrowReturnShape::UnsupportedForm),
    }
}

/// A statement-free block's tail, classified as a vec-slice source. The
/// [`classify_string_slice_block`] twin.
fn classify_vec_slice_block(
    b: &Block,
    ref_params: &HashSet<String>,
    ref_locals: &HashSet<String>,
    slice_locals: &HashSet<String>,
) -> Option<BorrowReturnShape> {
    if !b.stmts.is_empty() {
        return Some(BorrowReturnShape::UnsupportedForm);
    }
    match &b.final_expr {
        Some(e) => classify_vec_slice_return(e, ref_params, ref_locals, slice_locals),
        None => Some(BorrowReturnShape::UnsupportedForm),
    }
}

/// Top-level `let s = <borrowable>.as_slice()` / `let s = <borrowable>[a..b]`
/// bindings — `s` carries the receiver's borrow and is itself a valid source.
/// The [`collect_string_slice_locals`] twin, and conservative in the same way:
/// only the body's direct statements are scanned, so a missed nested binding
/// yields at worst a false `UnsupportedForm`, never an unsound accept.
fn collect_vec_slice_locals(
    block: &Block,
    ref_params: &HashSet<String>,
    ref_locals: &HashSet<String>,
    out: &mut HashSet<String>,
) {
    for stmt in &block.stmts {
        if let StmtKind::Let { pattern, value, .. } = &stmt.kind {
            if let PatternKind::Binding(name) = &pattern.kind {
                let borrowable = match &value.kind {
                    ExprKind::MethodCall { object, method, .. }
                        if method == "as_slice" || method == "as_slice_mut" =>
                    {
                        classify_vec_slice_root(object, ref_params, ref_locals, out).is_none()
                    }
                    ExprKind::Index { object, .. } => {
                        classify_vec_slice_root(object, ref_params, ref_locals, out).is_none()
                    }
                    _ => false,
                };
                if borrowable {
                    out.insert(name.clone());
                }
            }
        }
    }
}

/// A statement-free block's tail, classified as a StringSlice source (a block
/// with preceding statements is `UnsupportedForm`, never miscompiled).
fn classify_string_slice_block(
    b: &Block,
    ref_params: &HashSet<String>,
    ref_locals: &HashSet<String>,
    slice_locals: &HashSet<String>,
) -> Option<BorrowReturnShape> {
    if !b.stmts.is_empty() {
        return Some(BorrowReturnShape::UnsupportedForm);
    }
    match &b.final_expr {
        Some(e) => classify_string_slice_return(e, ref_params, ref_locals, slice_locals),
        None => Some(BorrowReturnShape::UnsupportedForm),
    }
}

/// Classify the receiver root of a `recv.slice(..)` view: a borrowable source
/// → `None` (accept); an owned identifier / `self` → `DanglingSource`; a
/// non-identifier receiver (chained) → `UnsupportedForm`.
fn classify_slice_receiver_root(
    object: &Expr,
    ref_params: &HashSet<String>,
    ref_locals: &HashSet<String>,
    slice_locals: &HashSet<String>,
) -> Option<BorrowReturnShape> {
    match &object.kind {
        ExprKind::Identifier(n)
            if ref_params.contains(n) || ref_locals.contains(n) || slice_locals.contains(n) =>
        {
            None
        }
        ExprKind::SelfValue if ref_params.contains("self") => None,
        ExprKind::Identifier(_) | ExprKind::SelfValue => Some(BorrowReturnShape::DanglingSource),
        _ => Some(BorrowReturnShape::UnsupportedForm),
    }
}

/// Whether a `recv` in `recv.slice(..)` is a borrowable view source.
fn slice_receiver_is_borrowable(
    object: &Expr,
    ref_params: &HashSet<String>,
    ref_locals: &HashSet<String>,
    slice_locals: &HashSet<String>,
) -> bool {
    match &object.kind {
        ExprKind::Identifier(n) => {
            ref_params.contains(n) || ref_locals.contains(n) || slice_locals.contains(n)
        }
        ExprKind::SelfValue => ref_params.contains("self"),
        _ => false,
    }
}

/// Top-level `let w = <borrowable>.slice(a, b)` bindings — `w` carries the
/// receiver's borrow and is itself a valid view source. Conservative: only the
/// function body's direct statements are scanned (a missed nested binding at
/// worst yields a false `UnsupportedForm`, never an unsound accept).
fn collect_string_slice_locals(
    block: &Block,
    ref_params: &HashSet<String>,
    ref_locals: &HashSet<String>,
    out: &mut HashSet<String>,
) {
    for stmt in &block.stmts {
        if let StmtKind::Let { pattern, value, .. } = &stmt.kind {
            if let PatternKind::Binding(name) = &pattern.kind {
                if let ExprKind::MethodCall { object, method, .. } = &value.kind {
                    if method == "slice"
                        && slice_receiver_is_borrowable(object, ref_params, ref_locals, out)
                    {
                        out.insert(name.clone());
                    }
                }
            }
        }
    }
}

/// The root of a place reached by field, tuple-index and index projections
/// (`self`, or a local), or `None` when `e` is not such a place.
fn place_root(e: &Expr) -> Option<&Expr> {
    match &e.kind {
        ExprKind::Identifier(_) | ExprKind::SelfValue => Some(e),
        ExprKind::FieldAccess { object, .. }
        | ExprKind::TupleIndex { object, .. }
        | ExprKind::Index { object, .. } => place_root(object),
        _ => None,
    }
}

/// A borrow of the place `e`: pinned when its root is a borrow source, a
/// dangling borrow when its root is an owned local, and not yet supported
/// when `e` is not a place.
fn ref_place_shape(
    e: &Expr,
    ref_params: &HashSet<String>,
    ref_locals: &HashSet<String>,
) -> Option<BorrowReturnShape> {
    match place_root(e).map(|r| &r.kind) {
        Some(ExprKind::Identifier(n)) if ref_params.contains(n) || ref_locals.contains(n) => None,
        Some(ExprKind::SelfValue) if ref_params.contains("self") => None,
        Some(_) => Some(BorrowReturnShape::DanglingSource),
        None => Some(BorrowReturnShape::UnsupportedForm),
    }
}

/// The place a `for` iterable walks by borrow, when it does: `p.iter()`,
/// `p.keys()`, `p.values()`, any of them under `.enumerate()` or `.rev()`.
/// `into_iter`, `drain` and every other method are not borrowing walks.
fn borrowed_walk_place(e: &Expr) -> Option<&Expr> {
    let ExprKind::MethodCall {
        object,
        method,
        args,
        ..
    } = &e.kind
    else {
        return None;
    };
    if !args.is_empty() {
        return None;
    }
    match method.as_str() {
        "iter" | "keys" | "values" => Some(object),
        "enumerate" | "rev" => borrowed_walk_place(object),
        _ => None,
    }
}

/// Where a local's value comes from, for [`collect_core_ref_locals`].
enum CoreDef<'e> {
    /// `let x = e` or `x = e`.
    Value(&'e Expr),
    /// A pattern binding over the place `e` (`if let`, `while let`, `match`,
    /// `let … else`): v2 core §4.6 binds a `ref` into the place.
    Projection(&'e Expr),
}

/// v2 core §5.1 (B-2026-10-14-38, B-2026-10-14-71): the locals that hold a borrow of a `ref`
/// parameter, so returning one is pinned. A local qualifies when every
/// definition of its name is `ref <place>`, another such local, or a pattern
/// binding into a place, each rooted at a `ref` parameter or a qualifying
/// local: `let mut node = ref self.head; while let Some(n) = node.next { node
/// = n; } node`. The greatest fixpoint, since a loop's definitions refer to
/// each other; every chain starts at a `let`, which must be rooted, so the
/// set is sound. A name bound any other way (a `for` element, a
/// destructuring `let`) does not qualify.
fn collect_core_ref_locals(
    body: &Block,
    ref_params: &HashSet<String>,
    ref_locals: &mut HashSet<String>,
) {
    let mut defs: std::collections::HashMap<String, Vec<CoreDef<'_>>> =
        std::collections::HashMap::new();
    let mut poisoned: HashSet<String> = HashSet::new();
    core_defs_in_block(body, &mut defs, &mut poisoned);
    let mut set: HashSet<String> = defs
        .keys()
        .filter(|n| !poisoned.contains(*n) && !ref_locals.contains(*n))
        .cloned()
        .collect();
    loop {
        let all: HashSet<String> = ref_locals.union(&set).cloned().collect();
        let before = set.len();
        set.retain(|n| {
            defs[n].iter().all(|d| match d {
                CoreDef::Value(e) => match &e.kind {
                    ExprKind::Unary {
                        op: UnaryOp::Ref | UnaryOp::MutRef,
                        operand,
                    } => ref_place_shape(operand, ref_params, &all).is_none(),
                    ExprKind::Identifier(m) => ref_params.contains(m) || all.contains(m),
                    _ => false,
                },
                CoreDef::Projection(e) => ref_place_shape(e, ref_params, &all).is_none(),
            })
        });
        if set.len() == before {
            break;
        }
    }
    ref_locals.extend(set);
}

fn core_defs_in_block<'e>(
    b: &'e Block,
    defs: &mut std::collections::HashMap<String, Vec<CoreDef<'e>>>,
    poisoned: &mut HashSet<String>,
) {
    for stmt in &b.stmts {
        match &stmt.kind {
            StmtKind::Let { pattern, value, .. } => {
                if let PatternKind::Binding(name) = &pattern.kind {
                    defs.entry(name.clone())
                        .or_default()
                        .push(CoreDef::Value(value));
                } else {
                    poisoned.extend(pattern.binding_names());
                }
                core_defs_in_expr(value, defs, poisoned);
            }
            StmtKind::LetElse {
                pattern,
                value,
                else_block,
                ..
            } => {
                for n in pattern.binding_names() {
                    defs.entry(n).or_default().push(CoreDef::Projection(value));
                }
                core_defs_in_expr(value, defs, poisoned);
                core_defs_in_block(else_block, defs, poisoned);
            }
            StmtKind::Assign { target, value } => {
                if let ExprKind::Identifier(name) = &target.kind {
                    defs.entry(name.clone())
                        .or_default()
                        .push(CoreDef::Value(value));
                }
                core_defs_in_expr(value, defs, poisoned);
            }
            StmtKind::Expr(e) | StmtKind::CompoundAssign { value: e, .. } => {
                core_defs_in_expr(e, defs, poisoned)
            }
            StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                core_defs_in_block(body, defs, poisoned)
            }
            StmtKind::LetUninit { name, .. } => {
                poisoned.insert(name.clone());
            }
            StmtKind::MultiAssign { .. } => {}
        }
    }
    if let Some(e) = &b.final_expr {
        core_defs_in_expr(e, defs, poisoned);
    }
}

fn core_defs_in_expr<'e>(
    e: &'e Expr,
    defs: &mut std::collections::HashMap<String, Vec<CoreDef<'e>>>,
    poisoned: &mut HashSet<String>,
) {
    match &e.kind {
        ExprKind::IfLet { pattern, value, .. } | ExprKind::WhileLet { pattern, value, .. } => {
            for n in pattern.binding_names() {
                defs.entry(n).or_default().push(CoreDef::Projection(value));
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            for arm in arms {
                for n in arm.pattern.binding_names() {
                    defs.entry(n)
                        .or_default()
                        .push(CoreDef::Projection(scrutinee));
                }
            }
        }
        // A `for` over a borrowing walk of a place lends each element of it
        // (B-2026-10-14-60): `for (k, it) in v.iter()` with `v: ref Vec[..]`.
        ExprKind::For {
            pattern, iterable, ..
        } => match borrowed_walk_place(iterable) {
            Some(place) => {
                for n in pattern.binding_names() {
                    defs.entry(n).or_default().push(CoreDef::Projection(place));
                }
            }
            None => poisoned.extend(pattern.binding_names()),
        },
        _ => {}
    }
    for_each_subblock(e, &mut |b| core_defs_in_block(b, defs, poisoned));
}

/// Names bound by `let <name> = <call to a ref-returning fn>;` anywhere in
/// the block tree. These are ref-locals — valid borrow-return sources.
fn collect_ref_locals(block: &Block, ref_fns: &HashSet<String>, out: &mut HashSet<String>) {
    for stmt in &block.stmts {
        if let StmtKind::Let { pattern, value, .. } = &stmt.kind {
            if let PatternKind::Binding(name) = &pattern.kind {
                if let ExprKind::Call { callee, .. } = &value.kind {
                    if let ExprKind::Identifier(fname) = &callee.kind {
                        if ref_fns.contains(fname) {
                            out.insert(name.clone());
                        }
                    }
                }
            }
        }
        collect_ref_locals_in_stmt(stmt, ref_fns, out);
    }
    if let Some(e) = &block.final_expr {
        collect_ref_locals_in_expr(e, ref_fns, out);
    }
}

fn collect_ref_locals_in_stmt(stmt: &Stmt, ref_fns: &HashSet<String>, out: &mut HashSet<String>) {
    match &stmt.kind {
        StmtKind::MultiAssign { .. } => unreachable!(
            "StmtKind::MultiAssign is removed by the desugar pass before reaching this phase"
        ),
        StmtKind::Let { value, .. }
        | StmtKind::LetElse { value, .. }
        | StmtKind::Expr(value)
        | StmtKind::Assign { value, .. }
        | StmtKind::CompoundAssign { value, .. } => collect_ref_locals_in_expr(value, ref_fns, out),
        StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
            collect_ref_locals(body, ref_fns, out)
        }
        StmtKind::LetUninit { .. } => {}
    }
}

fn collect_ref_locals_in_expr(e: &Expr, ref_fns: &HashSet<String>, out: &mut HashSet<String>) {
    for_each_subblock(e, &mut |b| collect_ref_locals(b, ref_fns, out));
}

/// True when `te` is a container / wrapper type that STORES a `ref` in an
/// element / payload position (`Vec[ref T]`, `Option[ref T]`,
/// `Result[ref T, E]`, a tuple with a `ref` member) — a "borrowed collection"
/// per design.md Feature 4 Part 3. A top-level `ref T` / `mut ref T` return is
/// Tier-1 (handled separately), so it is not reported here.
fn type_expr_has_ref_element(te: &TypeExpr) -> bool {
    match &te.kind {
        TypeKind::Path(p) => p.generic_args.as_ref().is_some_and(|args| {
            args.iter().any(|a| match a {
                GenericArg::Type(inner) => type_expr_contains_ref(inner),
                _ => false,
            })
        }),
        TypeKind::Tuple(elems) => elems.iter().any(type_expr_contains_ref),
        _ => false,
    }
}

/// The first type argument of a generic return type: `T` of `Vec[T]`,
/// `Option[T]` or `Result[T, E]`.
fn first_type_arg(te: &TypeExpr) -> Option<&TypeExpr> {
    let TypeKind::Path(p) = &te.kind else {
        return None;
    };
    p.generic_args.as_ref()?.iter().find_map(|a| match a {
        GenericArg::Type(t) => Some(t),
        _ => None,
    })
}

/// `te` is (or transitively contains, through a generic arg / tuple member) a
/// `ref` / `mut ref`. The element-position recursion of `type_expr_has_ref_-
/// element` so `Vec[Option[ref T]]` is seen too.
fn type_expr_contains_ref(te: &TypeExpr) -> bool {
    matches!(te.kind, TypeKind::Ref(_) | TypeKind::MutRef(_)) || type_expr_has_ref_element(te)
}

/// Collect the argument of every `<container>.push(arg)` call on the named
/// container binding, anywhere in the block tree — the element sources of a
/// returned borrowed collection (B-2026-07-11-30).
fn collect_container_push_sources<'e>(block: &'e Block, container: &str, out: &mut Vec<&'e Expr>) {
    for stmt in &block.stmts {
        match &stmt.kind {
            StmtKind::Let { value, .. }
            | StmtKind::LetElse { value, .. }
            | StmtKind::Expr(value)
            | StmtKind::Assign { value, .. }
            | StmtKind::CompoundAssign { value, .. } => {
                collect_container_push_sources_in_expr(value, container, out)
            }
            StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                collect_container_push_sources(body, container, out)
            }
            StmtKind::MultiAssign { .. } | StmtKind::LetUninit { .. } => {}
        }
    }
    if let Some(e) = &block.final_expr {
        collect_container_push_sources_in_expr(e, container, out);
    }
}

fn collect_container_push_sources_in_expr<'e>(
    e: &'e Expr,
    container: &str,
    out: &mut Vec<&'e Expr>,
) {
    if let ExprKind::MethodCall {
        object,
        method,
        args,
        ..
    } = &e.kind
    {
        if method == "push" && args.len() == 1 {
            if let ExprKind::Identifier(n) = &object.kind {
                if n == container {
                    out.push(&args[0].value);
                }
            }
        }
    }
    for_each_subblock(e, &mut |b| {
        collect_container_push_sources(b, container, out)
    });
}

/// Collect every `return e;` expression in the block tree.
/// Whether a value of type `t` can hold a reference: a reference, a view
/// (`StringSlice`, `Slice`) or an aggregate containing one.
fn type_holds_ref(t: &Type) -> bool {
    match t {
        Type::Ref(_) | Type::MutRef(_) => true,
        Type::Named { name, args } => {
            matches!(name.as_str(), "StringSlice" | "Str" | "Slice")
                || args.iter().any(type_holds_ref)
        }
        Type::Tuple(ts) => ts.iter().any(type_holds_ref),
        _ => false,
    }
}

/// v2 core §5.4 (B-2026-10-19-37): the places a `-> ref T` or
/// `-> mut ref T` function's result names at its return sites. The result
/// lends them, so none is a move out of its collection.
/// One place [`core_bare_ref_returns`] found.
#[derive(Clone, Copy)]
pub(crate) struct BareRefReturn {
    pub span: crate::token::Span,
    /// The slot is `mut ref`.
    pub is_mut: bool,
    /// The place is a call argument (`Some(p.first)`), where `ref` is not
    /// written (design.md Feature 4): a `mut ref` slot takes the call-site
    /// `mut` marker, and a `ref` slot is borrowed into a binding first.
    pub in_arg: bool,
}

/// design.md § returning a reference, B-2026-10-22-28: the places a
/// function returns bare where its return type has a `ref` / `mut ref`
/// slot (the whole type, or a slot of an `Option` or a tuple). "The `ref` is written": a bare
/// `p.first` there would move out of a borrowed place, so each is
/// reported once its type is known not to be `Copy`.
pub(super) fn core_bare_ref_returns(f: &Function) -> Vec<BareRefReturn> {
    let mut out = Vec::new();
    let Some(ret) = &f.return_type else {
        return out;
    };
    let mut returns: Vec<&Expr> = Vec::new();
    collect_return_exprs_in_block(&f.body, &mut returns);
    if let Some(tail) = &f.body.final_expr {
        if !matches!(tail.kind, ExprKind::Return(_)) {
            returns.push(tail);
        }
    }
    for e in returns {
        bare_ref_slots(e, ret, false, &mut out);
    }
    out
}

fn bare_ref_slots(e: &Expr, ty: &TypeExpr, in_arg: bool, out: &mut Vec<BareRefReturn>) {
    match &e.kind {
        ExprKind::Return(Some(v)) => return bare_ref_slots(v, ty, in_arg, out),
        ExprKind::Block(b) => {
            if let Some(t) = &b.final_expr {
                bare_ref_slots(t, ty, in_arg, out);
            }
            return;
        }
        ExprKind::If {
            then_block,
            else_branch,
            ..
        }
        | ExprKind::IfLet {
            then_block,
            else_branch,
            ..
        } => {
            if let Some(t) = &then_block.final_expr {
                bare_ref_slots(t, ty, in_arg, out);
            }
            if let Some(eb) = else_branch {
                bare_ref_slots(eb, ty, in_arg, out);
            }
            return;
        }
        ExprKind::Match { arms, .. } => {
            for arm in arms {
                bare_ref_slots(&arm.body, ty, in_arg, out);
            }
            return;
        }
        _ => {}
    }
    match &ty.kind {
        TypeKind::Ref(_) | TypeKind::MutRef(_) => {
            if matches!(
                e.kind,
                ExprKind::Index { .. } | ExprKind::FieldAccess { .. } | ExprKind::TupleIndex { .. }
            ) {
                out.push(BareRefReturn {
                    span: e.span,
                    is_mut: matches!(ty.kind, TypeKind::MutRef(_)),
                    in_arg,
                });
            }
        }
        TypeKind::Tuple(slots) => {
            if let ExprKind::Tuple(elems) = &e.kind {
                if elems.len() == slots.len() {
                    for (el, slot) in elems.iter().zip(slots) {
                        bare_ref_slots(el, slot, in_arg, out);
                    }
                }
            }
        }
        TypeKind::Path(p) if p.segments.last().is_some_and(|s| s == "Option") => {
            let Some([GenericArg::Type(inner)]) = p.generic_args.as_deref() else {
                return;
            };
            if let ExprKind::Call { callee, args } = &e.kind {
                let is_some = matches!(
                    &callee.kind,
                    ExprKind::Identifier(n) if n == "Some"
                ) || matches!(
                    &callee.kind,
                    ExprKind::Path { segments, .. } if segments.last().is_some_and(|s| s == "Some")
                );
                // `Some(mut self[key])` already lends its place mutably.
                if is_some && args.len() == 1 && !args[0].mut_marker {
                    bare_ref_slots(&args[0].value, inner, true, out);
                }
            }
        }
        _ => {}
    }
}

pub(super) fn core_returned_places(f: &Function) -> HashSet<SpanKey> {
    let mut out = HashSet::new();
    let Some(ret) = &f.return_type else {
        return out;
    };
    if !matches!(ret.kind, TypeKind::Ref(_) | TypeKind::MutRef(_)) {
        return out;
    }
    let mut returns: Vec<&Expr> = Vec::new();
    collect_return_exprs_in_block(&f.body, &mut returns);
    if let Some(tail) = &f.body.final_expr {
        if !matches!(tail.kind, ExprKind::Return(_)) {
            returns.push(tail);
        }
    }
    for e in returns {
        returned_places(e, &mut out);
    }
    out
}

fn returned_places(e: &Expr, out: &mut HashSet<SpanKey>) {
    match &e.kind {
        ExprKind::Index { .. } | ExprKind::FieldAccess { .. } | ExprKind::TupleIndex { .. } => {
            out.insert(SpanKey::from_span(&e.span));
        }
        ExprKind::Block(b) => {
            if let Some(t) = &b.final_expr {
                returned_places(t, out);
            }
        }
        ExprKind::If {
            then_block,
            else_branch,
            ..
        }
        | ExprKind::IfLet {
            then_block,
            else_branch,
            ..
        } => {
            if let Some(t) = &then_block.final_expr {
                returned_places(t, out);
            }
            if let Some(eb) = else_branch {
                returned_places(eb, out);
            }
        }
        ExprKind::Match { arms, .. } => {
            for arm in arms {
                returned_places(&arm.body, out);
            }
        }
        _ => {}
    }
}

fn collect_return_exprs_in_block<'e>(block: &'e Block, out: &mut Vec<&'e Expr>) {
    for stmt in &block.stmts {
        collect_return_exprs_in_stmt(stmt, out);
    }
    if let Some(e) = &block.final_expr {
        collect_return_exprs_in_expr(e, out);
    }
}

fn collect_return_exprs_in_stmt<'e>(stmt: &'e Stmt, out: &mut Vec<&'e Expr>) {
    match &stmt.kind {
        StmtKind::MultiAssign { .. } => unreachable!(
            "StmtKind::MultiAssign is removed by the desugar pass before reaching this phase"
        ),
        StmtKind::Let { value, .. }
        | StmtKind::LetElse { value, .. }
        | StmtKind::Expr(value)
        | StmtKind::Assign { value, .. }
        | StmtKind::CompoundAssign { value, .. } => collect_return_exprs_in_expr(value, out),
        StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
            collect_return_exprs_in_block(body, out)
        }
        StmtKind::LetUninit { .. } => {}
    }
}

fn collect_return_exprs_in_expr<'e>(e: &'e Expr, out: &mut Vec<&'e Expr>) {
    if let ExprKind::Return(Some(inner)) = &e.kind {
        out.push(inner);
    }
    for_each_subblock(e, &mut |b| collect_return_exprs_in_block(b, out));
}

/// Invoke `f` on every `Block` directly nested in `e` (one level; the
/// callbacks recurse). Covers the control-flow and grouping expression
/// forms that can host statements / nested returns. Leaf and operator
/// expressions have no nested blocks and are ignored — a missed nested
/// `return` only degrades a dangling diagnostic to the codegen-level
/// verifier error, never a soundness gap (see module docs).
fn for_each_subblock<'e>(e: &'e Expr, f: &mut dyn FnMut(&'e Block)) {
    match &e.kind {
        ExprKind::Block(b)
        | ExprKind::Unsafe(b)
        | ExprKind::Try(b)
        | ExprKind::Seq(b)
        | ExprKind::Par(b) => f(b),
        ExprKind::If {
            then_block,
            else_branch,
            ..
        }
        | ExprKind::IfLet {
            then_block,
            else_branch,
            ..
        } => {
            f(then_block);
            if let Some(eb) = else_branch {
                collect_else_branch(eb, f);
            }
        }
        ExprKind::While { body, .. }
        | ExprKind::WhileLet { body, .. }
        | ExprKind::For { body, .. }
        | ExprKind::Loop { body, .. }
        | ExprKind::LabeledBlock { body, .. } => f(body),
        ExprKind::Match { arms, .. } => {
            for arm in arms {
                collect_return_exprs_or_blocks_in_match_arm(arm, f);
            }
        }
        _ => {}
    }
}

fn collect_else_branch<'e>(eb: &'e Expr, f: &mut dyn FnMut(&'e Block)) {
    // An `else` is either a `Block` expr or a chained `if` — recurse so
    // `else if` chains are covered.
    match &eb.kind {
        ExprKind::Block(b) => f(b),
        _ => for_each_subblock(eb, f),
    }
}

fn collect_return_exprs_or_blocks_in_match_arm<'e>(
    arm: &'e MatchArm,
    f: &mut dyn FnMut(&'e Block),
) {
    // A match-arm body is an expression; route through the block hook so
    // both block-bodied and expression-bodied arms are visited.
    match &arm.body.kind {
        ExprKind::Block(b) => f(b),
        _ => for_each_subblock(&arm.body, f),
    }
}
