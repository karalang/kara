//! FE-2 — `GpuSafe` structural type-check for `#[gpu]` functions.
//!
//! A `#[gpu]`-annotated function (design.md § GPU Subset Constraints) may
//! use only the GPU-compatible subset of types: primitive scalars, the
//! portable-SIMD `Vector`, fixed-size `Array[T, N]`, tuples, `Option`/
//! `Result` over GPU-safe inners, and user structs/enums *all* of whose
//! fields/variant payloads are themselves GPU-safe. The GPU execution model
//! is allocation-free and reference-free, so the following are rejected:
//!
//! - **Heap-allocated types** — `String` (`Type::Str`), `Vec[T]`,
//!   `VecDeque[T]`, `Map`/`SortedMap`, `Set`/`SortedSet`.
//! - **RC / `shared` reference types** — `shared struct`/`shared enum`
//!   (`Type::Shared`), `Rc[T]`, `Arc[T]`, `Weak[T]`.
//! - **Any aggregate transitively containing one** — a struct with a
//!   `String` field, `Option[String]`, `(i64, Vec[i64])`, etc.
//!
//! The check is purely structural — it mirrors the auto-derived `GpuSafe`
//! trait (compatibility is "all the way down"). FE-2 runs over a `#[gpu]`
//! function's **parameter and return types** (the signature boundary, the
//! highest-confidence enforcement point); **FE-2b** ([`Self::check_gpu_safe_bindings`])
//! extends the same predicate to the function's **local `let`-binding types**.
//!
//! A heap local in a `#[gpu]` body almost always originates from an
//! allocating call (`Vec.new()`, `"…".to_string()`, a collection literal),
//! which FE-4 also rejects through the `allocates(Heap)` effect — but FE-2b
//! gives a precise *type-level* diagnostic at the binding and catches the
//! rare non-allocating heap local. Each binding's type is read from the
//! populated `expr_types` table (value-bearing `let`) or lowered from its
//! annotation (`let x: T;`), so it runs after the body is checked. The walk
//! descends control-flow blocks (`if`/`match`/loops/block-exprs) to reach
//! nested bindings; a `let` buried inside a non-control-flow sub-expression
//! (e.g. a block-expr passed as a call argument) is left to FE-4's effect
//! backstop. Closures/recursion (FE-3) and `panics`/IO effects (FE-4) are
//! separate enforcement axes.
//!
//! Generic type parameters of the `#[gpu]` function itself are treated as
//! GPU-safe here (deferred): a generic GPU function declares `T: GpuSafe`
//! and that bound is what makes the instantiation safe — enforcing the
//! bound is FE-3's job. When recursing through a *concrete* generic
//! aggregate (`Wrapper[String]`), the type arguments are substituted into
//! the field types so `String` is still caught.

use super::types::{Type, VariantTypeInfo};
use crate::ast::{Block, Expr, ExprKind, Function, Stmt, StmtKind, TypeExpr};
use crate::resolver::SpanKey;
use crate::token::Span;
use crate::typechecker::TypeErrorKind;
use std::collections::{HashMap, HashSet};

/// Why a type is not GPU-safe, plus the field/element path from the
/// signature/binding type down to the offending leaf. `path` reads
/// outer→inner, e.g. `["Particle.name"]` for a `Particle` param whose
/// `name: String` field is the culprit; empty when the offending type *is*
/// the top-level type itself.
struct GpuUnsafe {
    /// Display name of the offending leaf type (`String`, `Vec`, `Arc`, …).
    leaf: String,
    /// Human category for the `= note:` line.
    reason: GpuUnsafeReason,
    path: Vec<String>,
}

enum GpuUnsafeReason {
    Heap,
    SharedRc,
}

impl GpuUnsafeReason {
    fn note(&self) -> &'static str {
        match self {
            GpuUnsafeReason::Heap => {
                "GPU functions cannot use heap-allocated types — the GPU \
                 execution model has no host heap"
            }
            GpuUnsafeReason::SharedRc => {
                "GPU functions cannot use reference-counted / `shared` types \
                 — the GPU execution model has no shared host memory"
            }
        }
    }

    fn hint(&self) -> &'static str {
        match self {
            GpuUnsafeReason::Heap => {
                "use a fixed-size `Array[T, N]` (or a struct of primitives) \
                 for GPU-compatible data"
            }
            GpuUnsafeReason::SharedRc => {
                "pass the underlying value by `Array[T, N]` / struct-of-\
                 primitives instead of a `shared` / `Rc` / `Arc` handle"
            }
        }
    }
}

/// A local binding's type to check (FE-2b), captured during the immutable
/// body walk and resolved in the mutable emit pass. A value-bearing `let`
/// carries the already-lowered type recorded in `expr_types`; an
/// uninitialised `let x: T;` carries its annotation, lowered with the
/// function's generic scope at emit time.
enum BindingTy<'b> {
    Lowered(Type),
    Annot(&'b TypeExpr),
}

impl<'a> super::TypeChecker<'a> {
    /// FE-2 entry point. Called from `check_function` for every `#[gpu]`
    /// function (free fn or impl method) after its parameter and return
    /// types have been lowered. Walks the signature types and emits an
    /// `E0801` `GpuNotSafe` diagnostic at the offending parameter/return
    /// span for each GPU-incompatible type. Local-binding checking rides
    /// the same predicate via [`Self::check_gpu_safe_binding`], driven from
    /// the let-statement walk.
    pub(super) fn check_gpu_safe_signature(&mut self, f: &Function, generic_scope: &[String]) {
        // Parameters.
        for (idx, param) in f.params.iter().enumerate() {
            let ty = self.lower_type_expr(&param.ty, generic_scope);
            // GPU-LBM-6 stencil form: the *first* parameter may be a whole
            // `Vec[S]` buffer — the kernel reads neighbours `buf[j].field`. On
            // the GPU this maps to the bound storage buffers, not the host heap,
            // so a `Vec[S]` here is GPU-safe iff its element `S` is. Check the
            // element (an ordinary struct-of-primitives), not the `Vec` wrapper.
            // (An element-wise kernel's first parameter is the struct `S` itself,
            // never a `Vec`, so this never masks a real violation there.)
            if idx == 0 {
                if let Type::Named { name, args } = &ty {
                    if name == "Vec" && args.len() == 1 {
                        if let Some(bad) = self.gpu_unsafe_reason(&args[0]) {
                            self.emit_gpu_not_safe(&bad, param.ty.span.clone(), "parameter");
                        }
                        continue;
                    }
                }
            }
            if let Some(bad) = self.gpu_unsafe_reason(&ty) {
                self.emit_gpu_not_safe(&bad, param.ty.span.clone(), "parameter");
            }
        }
        // Return type.
        if let Some(ret) = &f.return_type {
            let ty = self.lower_type_expr(ret, generic_scope);
            if let Some(bad) = self.gpu_unsafe_reason(&ty) {
                self.emit_gpu_not_safe(&bad, ret.span.clone(), "return type");
            }
        }
    }

    /// FE-2b entry point. Called from `check_function` for every `#[gpu]`
    /// function after its body has been type-checked (so `expr_types` carries
    /// each binding's value type). Walks the body's `let` bindings — at any
    /// control-flow nesting depth — and emits an `E0801` `GpuNotSafe`
    /// diagnostic for any binding whose type is GPU-incompatible.
    ///
    /// Two-phase to satisfy the borrow checker: the walk reads `expr_types`
    /// (`&self`) and collects `(span, type)` pairs, then the emit pass lowers
    /// any annotation and reports (`&mut self`).
    pub(super) fn check_gpu_safe_bindings(&mut self, body: &Block, generic_scope: &[String]) {
        let mut bindings: Vec<(Span, BindingTy)> = Vec::new();
        self.collect_gpu_let_bindings(body, &mut bindings);
        for (span, bty) in bindings {
            let ty = match bty {
                BindingTy::Lowered(t) => t,
                BindingTy::Annot(te) => self.lower_type_expr(te, generic_scope),
            };
            if let Some(bad) = self.gpu_unsafe_reason(&ty) {
                self.emit_gpu_not_safe_binding(&bad, span);
            }
        }
    }

    fn collect_gpu_let_bindings<'b>(&self, block: &'b Block, out: &mut Vec<(Span, BindingTy<'b>)>) {
        for s in &block.stmts {
            self.collect_let_in_stmt(s, out);
        }
        if let Some(fe) = &block.final_expr {
            self.collect_let_in_expr(fe, out);
        }
    }

    fn collect_let_in_stmt<'b>(&self, s: &'b Stmt, out: &mut Vec<(Span, BindingTy<'b>)>) {
        match &s.kind {
            StmtKind::Let { value, ty, .. } => {
                if let Some(bty) = self.binding_ty_for(value, ty.as_ref()) {
                    out.push((s.span.clone(), bty));
                }
                self.collect_let_in_expr(value, out);
            }
            StmtKind::LetElse {
                value,
                ty,
                else_block,
                ..
            } => {
                if let Some(bty) = self.binding_ty_for(value, ty.as_ref()) {
                    out.push((s.span.clone(), bty));
                }
                self.collect_let_in_expr(value, out);
                self.collect_gpu_let_bindings(else_block, out);
            }
            // Uninitialised `let x: T;` — no value to consult, so the
            // annotation is the binding type (lowered in the emit pass).
            StmtKind::LetUninit { ty, .. } => out.push((s.span.clone(), BindingTy::Annot(ty))),
            StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                self.collect_gpu_let_bindings(body, out)
            }
            StmtKind::Assign { value, .. } | StmtKind::CompoundAssign { value, .. } => {
                self.collect_let_in_expr(value, out)
            }
            StmtKind::Expr(e) => self.collect_let_in_expr(e, out),
            // Desugared away before this phase.
            StmtKind::MultiAssign { .. } => {}
        }
    }

    /// Recurse into the *block-bearing* expression forms only — those are the
    /// sole places a nested `let` statement can appear. Leaf expressions carry
    /// no statements, so they are not walked (see the module-doc note on the
    /// call-argument-block-expr limitation, which FE-4 backstops).
    fn collect_let_in_expr<'b>(&self, e: &'b Expr, out: &mut Vec<(Span, BindingTy<'b>)>) {
        match &e.kind {
            ExprKind::Block(b) => self.collect_gpu_let_bindings(b, out),
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
                self.collect_gpu_let_bindings(then_block, out);
                if let Some(eb) = else_branch {
                    self.collect_let_in_expr(eb, out);
                }
            }
            ExprKind::Match { arms, .. } => {
                for arm in arms {
                    if let Some(g) = &arm.guard {
                        self.collect_let_in_expr(g, out);
                    }
                    self.collect_let_in_expr(&arm.body, out);
                }
            }
            ExprKind::While { body, .. }
            | ExprKind::WhileLet { body, .. }
            | ExprKind::For { body, .. }
            | ExprKind::Loop { body, .. }
            | ExprKind::LabeledBlock { body, .. } => self.collect_gpu_let_bindings(body, out),
            _ => {}
        }
    }

    /// A value-bearing `let`'s type comes from the checked value (`expr_types`);
    /// if the value has no recorded type, fall back to the annotation. Returns
    /// `None` when neither is available (treated as safe — an `Error`/unknown
    /// binding).
    fn binding_ty_for<'b>(&self, value: &Expr, ann: Option<&'b TypeExpr>) -> Option<BindingTy<'b>> {
        if let Some(t) = self.expr_types.get(&SpanKey::from_span(&value.span)) {
            Some(BindingTy::Lowered(t.clone()))
        } else {
            ann.map(BindingTy::Annot)
        }
    }

    /// FE-3c entry point. Called from `closure_type_with_capture_inference`
    /// for every closure typed inside a `#[gpu]` function. A closure may not
    /// capture *host* state (design.md § GPU Subset Constraints — "Closures
    /// that capture from host memory"); the type-based rule is that a captured
    /// outer binding whose type is not `GpuSafe` is a host capture. Capturing
    /// only `GpuSafe` values (or nothing) is fine — those lower as kernel
    /// constants.
    ///
    /// FE-2 (params) and FE-2b (local `let`s) already make most in-scope
    /// bindings `GpuSafe`, so this mainly catches the residual: a non-`let`
    /// pattern binding (a `for` / `match` / `if let` variable over a
    /// non-`GpuSafe` scrutinee) captured by a closure. Each captured outer
    /// name is reported once, at the closure span.
    pub(super) fn check_gpu_closure_captures(
        &mut self,
        closure_span: &Span,
        closure_param_names: &[String],
        body: &Expr,
        outer_bindings: &HashMap<String, Type>,
    ) {
        let mut shadows: Vec<HashSet<String>> = vec![closure_param_names.iter().cloned().collect()];
        let mut seen: HashSet<String> = HashSet::new();
        let mut found: Vec<(String, GpuUnsafe)> = Vec::new();
        self.gpu_collect_captures(body, outer_bindings, &mut shadows, &mut seen, &mut found);
        for (name, bad) in found {
            self.emit_gpu_host_capture(closure_span.clone(), &name, &bad);
        }
    }

    /// Shadow-aware free-variable walk over a closure body. Records, once per
    /// name, each captured outer binding (present in `outer`, not shadowed by
    /// a closure param / inner `let` / nested pattern) whose type is not
    /// `GpuSafe`. Uniform traversal (unlike the moded once-callability walker
    /// in `closures.rs`): every sub-expression is walked the same way; only
    /// identifiers and shadow-introducing forms carry logic. The exhaustive
    /// match makes the compiler enforce that no expression form is missed.
    fn gpu_collect_captures(
        &self,
        expr: &Expr,
        outer: &HashMap<String, Type>,
        shadows: &mut Vec<HashSet<String>>,
        seen: &mut HashSet<String>,
        out: &mut Vec<(String, GpuUnsafe)>,
    ) {
        match &expr.kind {
            ExprKind::Identifier(name) => self.note_capture(name, outer, shadows, seen, out),
            ExprKind::SelfValue => self.note_capture("self", outer, shadows, seen, out),

            // Leaves — no captured sub-expressions. (An f-string's interpolated
            // exprs are not walked, mirroring the once-callability walker; a
            // captured heap value there is left to FE-4's effect backstop.)
            ExprKind::Integer(..)
            | ExprKind::Float(..)
            | ExprKind::Bool(..)
            | ExprKind::CharLit(..)
            | ExprKind::ByteLit(..)
            | ExprKind::StringLit(..)
            | ExprKind::MultiStringLit(..)
            | ExprKind::InterpolatedStringLit(..)
            | ExprKind::CStringLit { .. }
            | ExprKind::Path { .. }
            | ExprKind::SelfType
            | ExprKind::PipePlaceholder
            | ExprKind::OffsetOf { .. }
            | ExprKind::Error
            | ExprKind::Break { value: None, .. }
            | ExprKind::Continue { .. }
            | ExprKind::Return(None) => {}

            ExprKind::Binary { left, right, .. }
            | ExprKind::Pipe { left, right }
            | ExprKind::NilCoalesce { left, right } => {
                self.gpu_collect_captures(left, outer, shadows, seen, out);
                self.gpu_collect_captures(right, outer, shadows, seen, out);
            }
            ExprKind::Unary { operand, .. } => {
                self.gpu_collect_captures(operand, outer, shadows, seen, out)
            }
            ExprKind::Cast { expr: inner, .. } | ExprKind::Question(inner) => {
                self.gpu_collect_captures(inner, outer, shadows, seen, out)
            }
            ExprKind::Break { value: Some(v), .. } | ExprKind::Return(Some(v)) => {
                self.gpu_collect_captures(v, outer, shadows, seen, out)
            }

            ExprKind::Call { callee, args } => {
                self.gpu_collect_captures(callee, outer, shadows, seen, out);
                for arg in args {
                    self.gpu_collect_captures(&arg.value, outer, shadows, seen, out);
                }
            }
            ExprKind::MethodCall { object, args, .. } => {
                self.gpu_collect_captures(object, outer, shadows, seen, out);
                for arg in args {
                    self.gpu_collect_captures(&arg.value, outer, shadows, seen, out);
                }
            }
            ExprKind::OptionalChain { object, args, .. } => {
                self.gpu_collect_captures(object, outer, shadows, seen, out);
                if let Some(arg_list) = args {
                    for arg in arg_list {
                        self.gpu_collect_captures(&arg.value, outer, shadows, seen, out);
                    }
                }
            }
            ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
                self.gpu_collect_captures(object, outer, shadows, seen, out)
            }
            ExprKind::Index { object, index } => {
                self.gpu_collect_captures(object, outer, shadows, seen, out);
                self.gpu_collect_captures(index, outer, shadows, seen, out);
            }
            ExprKind::Range { start, end, .. } => {
                if let Some(s) = start {
                    self.gpu_collect_captures(s, outer, shadows, seen, out);
                }
                if let Some(e) = end {
                    self.gpu_collect_captures(e, outer, shadows, seen, out);
                }
            }
            ExprKind::Tuple(es) | ExprKind::ArrayLiteral(es) => {
                for e in es {
                    self.gpu_collect_captures(e, outer, shadows, seen, out);
                }
            }
            ExprKind::PrefixCollectionLiteral { items, .. } => {
                for e in items {
                    self.gpu_collect_captures(e, outer, shadows, seen, out);
                }
            }
            ExprKind::RepeatLiteral { value, count, .. } => {
                self.gpu_collect_captures(value, outer, shadows, seen, out);
                self.gpu_collect_captures(count, outer, shadows, seen, out);
            }
            ExprKind::MapLiteral(entries) => {
                for (k, v) in entries {
                    self.gpu_collect_captures(k, outer, shadows, seen, out);
                    self.gpu_collect_captures(v, outer, shadows, seen, out);
                }
            }
            ExprKind::StructLiteral { fields, spread, .. } => {
                for f in fields {
                    self.gpu_collect_captures(&f.value, outer, shadows, seen, out);
                }
                if let Some(s) = spread {
                    self.gpu_collect_captures(s, outer, shadows, seen, out);
                }
            }

            ExprKind::Block(block) | ExprKind::Comptime(block) => {
                self.gpu_collect_captures_block(block, outer, shadows, seen, out)
            }
            ExprKind::Par(body)
            | ExprKind::Seq(body)
            | ExprKind::Unsafe(body)
            | ExprKind::Try(body) => {
                self.gpu_collect_captures_block(body, outer, shadows, seen, out)
            }
            ExprKind::Lock { body, .. }
            | ExprKind::Loop { body, .. }
            | ExprKind::LabeledBlock { body, .. } => {
                self.gpu_collect_captures_block(body, outer, shadows, seen, out)
            }
            ExprKind::Providers { bindings, body } => {
                for binding in bindings {
                    self.gpu_collect_captures(&binding.value, outer, shadows, seen, out);
                }
                self.gpu_collect_captures_block(body, outer, shadows, seen, out);
            }
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                self.gpu_collect_captures(condition, outer, shadows, seen, out);
                self.gpu_collect_captures_block(then_block, outer, shadows, seen, out);
                if let Some(eb) = else_branch {
                    self.gpu_collect_captures(eb, outer, shadows, seen, out);
                }
            }
            ExprKind::While {
                condition, body, ..
            } => {
                self.gpu_collect_captures(condition, outer, shadows, seen, out);
                self.gpu_collect_captures_block(body, outer, shadows, seen, out);
            }
            // Pattern-binding forms push the pattern's names as a shadow scope
            // over the guarded body (the bound names are the closure's locals,
            // not captures).
            ExprKind::IfLet {
                pattern,
                value,
                then_block,
                else_branch,
            } => {
                self.gpu_collect_captures(value, outer, shadows, seen, out);
                shadows.push(pattern.binding_names().into_iter().collect());
                self.gpu_collect_captures_block(then_block, outer, shadows, seen, out);
                shadows.pop();
                if let Some(eb) = else_branch {
                    self.gpu_collect_captures(eb, outer, shadows, seen, out);
                }
            }
            ExprKind::WhileLet {
                pattern,
                value,
                body,
                ..
            } => {
                self.gpu_collect_captures(value, outer, shadows, seen, out);
                shadows.push(pattern.binding_names().into_iter().collect());
                self.gpu_collect_captures_block(body, outer, shadows, seen, out);
                shadows.pop();
            }
            ExprKind::For {
                pattern,
                iterable,
                body,
                ..
            } => {
                self.gpu_collect_captures(iterable, outer, shadows, seen, out);
                shadows.push(pattern.binding_names().into_iter().collect());
                self.gpu_collect_captures_block(body, outer, shadows, seen, out);
                shadows.pop();
            }
            ExprKind::Match { scrutinee, arms } => {
                self.gpu_collect_captures(scrutinee, outer, shadows, seen, out);
                for arm in arms {
                    shadows.push(arm.pattern.binding_names().into_iter().collect());
                    if let Some(g) = &arm.guard {
                        self.gpu_collect_captures(g, outer, shadows, seen, out);
                    }
                    self.gpu_collect_captures(&arm.body, outer, shadows, seen, out);
                    shadows.pop();
                }
            }
            // A nested closure's params shadow within its body; its own
            // captures of `outer` are still this `#[gpu]` function's captures.
            ExprKind::Closure {
                params: nested_params,
                body: nested_body,
                ..
            } => {
                let mut nested_scope: HashSet<String> = HashSet::new();
                for p in nested_params {
                    for n in p.pattern.binding_names() {
                        nested_scope.insert(n);
                    }
                }
                shadows.push(nested_scope);
                self.gpu_collect_captures(nested_body, outer, shadows, seen, out);
                shadows.pop();
            }
        }
    }

    fn gpu_collect_captures_block(
        &self,
        block: &Block,
        outer: &HashMap<String, Type>,
        shadows: &mut Vec<HashSet<String>>,
        seen: &mut HashSet<String>,
        out: &mut Vec<(String, GpuUnsafe)>,
    ) {
        shadows.push(HashSet::new());
        for stmt in &block.stmts {
            match &stmt.kind {
                StmtKind::Let { pattern, value, .. } => {
                    self.gpu_collect_captures(value, outer, shadows, seen, out);
                    if let Some(top) = shadows.last_mut() {
                        top.extend(pattern.binding_names());
                    }
                }
                StmtKind::LetUninit { name, .. } => {
                    if let Some(top) = shadows.last_mut() {
                        top.insert(name.clone());
                    }
                }
                StmtKind::LetElse {
                    pattern,
                    value,
                    else_block,
                    ..
                } => {
                    self.gpu_collect_captures(value, outer, shadows, seen, out);
                    self.gpu_collect_captures_block(else_block, outer, shadows, seen, out);
                    if let Some(top) = shadows.last_mut() {
                        top.extend(pattern.binding_names());
                    }
                }
                StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                    self.gpu_collect_captures_block(body, outer, shadows, seen, out)
                }
                StmtKind::Assign { target, value } => {
                    self.gpu_collect_captures(value, outer, shadows, seen, out);
                    self.gpu_collect_captures(target, outer, shadows, seen, out);
                }
                StmtKind::CompoundAssign { target, value, .. } => {
                    self.gpu_collect_captures(value, outer, shadows, seen, out);
                    self.gpu_collect_captures(target, outer, shadows, seen, out);
                }
                StmtKind::Expr(e) => self.gpu_collect_captures(e, outer, shadows, seen, out),
                // Desugared away before this phase.
                StmtKind::MultiAssign { .. } => {}
            }
        }
        if let Some(tail) = &block.final_expr {
            self.gpu_collect_captures(tail, outer, shadows, seen, out);
        }
        shadows.pop();
    }

    /// Record `name` as a host capture iff it is an outer binding (in `outer`),
    /// not shadowed by a closure-local, not already reported, and its type is
    /// not `GpuSafe`.
    fn note_capture(
        &self,
        name: &str,
        outer: &HashMap<String, Type>,
        shadows: &[HashSet<String>],
        seen: &mut HashSet<String>,
        out: &mut Vec<(String, GpuUnsafe)>,
    ) {
        if shadows.iter().any(|s| s.contains(name)) {
            return;
        }
        let Some(ty) = outer.get(name) else {
            return; // a global / free fn / not an outer binding — not a capture
        };
        if !seen.insert(name.to_string()) {
            return; // report each captured name once
        }
        if let Some(bad) = self.gpu_unsafe_reason(ty) {
            out.push((name.to_string(), bad));
        }
    }

    fn emit_gpu_host_capture(&mut self, closure_span: Span, name: &str, bad: &GpuUnsafe) {
        let where_ = if bad.path.is_empty() {
            String::new()
        } else {
            format!(" (via {})", bad.path.join(" → "))
        };
        let message = format!(
            "a closure in a `#[gpu]` function may not capture host state: it captures \
             `{name}`, whose type `{leaf}`{where_} is not GPU-compatible. {note}. hint: \
             {hint}, or pass the value as a kernel argument instead of capturing it.",
            leaf = bad.leaf,
            note = bad.reason.note(),
            hint = bad.reason.hint(),
        );
        self.type_error(message, closure_span, TypeErrorKind::GpuNotSafe);
    }

    fn emit_gpu_not_safe_binding(&mut self, bad: &GpuUnsafe, span: Span) {
        let where_ = if bad.path.is_empty() {
            String::new()
        } else {
            format!(" (via {})", bad.path.join(" → "))
        };
        let message = format!(
            "`{leaf}` is not GPU-compatible{where_}; it is the type of a local \
             binding in a `#[gpu]` function. {note}. hint: {hint}",
            leaf = bad.leaf,
            note = bad.reason.note(),
            hint = bad.reason.hint(),
        );
        self.type_error(message, span, TypeErrorKind::GpuNotSafe);
    }

    fn emit_gpu_not_safe(&mut self, bad: &GpuUnsafe, span: crate::token::Span, position: &str) {
        let where_ = if bad.path.is_empty() {
            String::new()
        } else {
            format!(" (via {})", bad.path.join(" → "))
        };
        let message = format!(
            "`{leaf}` is not GPU-compatible{where_}; it appears in the \
             {position} of a `#[gpu]` function. {note}. hint: {hint}",
            leaf = bad.leaf,
            note = bad.reason.note(),
            hint = bad.reason.hint(),
        );
        self.type_error(message, span, TypeErrorKind::GpuNotSafe);
    }

    /// Structural GPU-safety predicate. Returns `None` when `ty` is
    /// GPU-safe, or `Some(GpuUnsafe)` describing the first offending leaf
    /// (with its field/element path) otherwise. `subs` carries the active
    /// generic substitution (struct/enum type-param → concrete arg) while
    /// recursing through aggregates.
    fn gpu_unsafe_reason(&self, ty: &Type) -> Option<GpuUnsafe> {
        let mut visited: Vec<String> = Vec::new();
        let subs: HashMap<String, Type> = HashMap::new();
        self.gpu_unsafe_walk(ty, &subs, &mut visited)
    }

    /// SL-1 — public boolean face of the structural predicate. `T: GpuSafe`
    /// (the built-in marker bound declared for generic `#[gpu]` functions,
    /// design.md § GpuSafe trait) is satisfied iff this returns `true` — the
    /// same "all the way down" walk the FE-2 signature check uses, so the
    /// explicit bound and the implicit `#[gpu]` constraint agree by
    /// construction. Exposed as `bool` because `GpuUnsafe` is module-private.
    pub(super) fn is_gpu_safe_type(&self, ty: &Type) -> bool {
        self.gpu_unsafe_reason(ty).is_none()
    }

    fn gpu_unsafe_walk(
        &self,
        ty: &Type,
        subs: &HashMap<String, Type>,
        visited: &mut Vec<String>,
    ) -> Option<GpuUnsafe> {
        match ty {
            // GPU-safe scalars and the never/unit/error types.
            Type::Int(_)
            | Type::UInt(_)
            | Type::Float(_)
            | Type::Bool
            | Type::Char
            | Type::Unit
            | Type::Never
            | Type::Error => None,

            // Heap string.
            Type::Str => Some(GpuUnsafe {
                leaf: "String".to_string(),
                reason: GpuUnsafeReason::Heap,
                path: Vec::new(),
            }),

            // RC / shared reference handles.
            Type::Shared(name) => Some(GpuUnsafe {
                leaf: format!("shared {name}"),
                reason: GpuUnsafeReason::SharedRc,
                path: Vec::new(),
            }),
            Type::Rc(_) => Some(GpuUnsafe {
                leaf: "Rc".to_string(),
                reason: GpuUnsafeReason::SharedRc,
                path: Vec::new(),
            }),
            Type::Arc(_) => Some(GpuUnsafe {
                leaf: "Arc".to_string(),
                reason: GpuUnsafeReason::SharedRc,
                path: Vec::new(),
            }),
            Type::Weak(_) => Some(GpuUnsafe {
                leaf: "Weak".to_string(),
                reason: GpuUnsafeReason::SharedRc,
                path: Vec::new(),
            }),

            // Aggregates that recurse element-wise. A borrow / fixed array /
            // SIMD vector / tuple is GPU-safe iff its element(s) are — `ref
            // Array[f64, 3]` is the canonical kernel parameter shape.
            Type::Array { element, .. }
            | Type::Vector { element, .. }
            | Type::Slice { element, .. }
            | Type::Ref(element)
            | Type::MutRef(element)
            | Type::Pointer { inner: element, .. } => self.gpu_unsafe_walk(element, subs, visited),

            Type::Tuple(types) => types
                .iter()
                .find_map(|t| self.gpu_unsafe_walk(t, subs, visited)),

            // A generic parameter of the `#[gpu]` function: substitute if we
            // are inside a concrete aggregate, otherwise treat as safe
            // (the `T: GpuSafe` bound — FE-3 — is what makes it sound).
            Type::TypeParam(name) => match subs.get(name) {
                Some(concrete) => self.gpu_unsafe_walk(concrete, subs, visited),
                None => None,
            },

            Type::Named { name, args } => self.gpu_unsafe_named(name, args, subs, visited),

            // Function-typed values (the `f: fn(T) -> T` kernel argument) and
            // everything else not enumerated: treated as safe at this slice.
            // Host-capturing closures are an FE-3 call-graph concern, not a
            // structural type one.
            _ => None,
        }
    }

    /// `Named` covers the built-in heap collections, `Option`/`Result`, and
    /// every user struct/enum (generic or not).
    fn gpu_unsafe_named(
        &self,
        name: &str,
        args: &[Type],
        subs: &HashMap<String, Type>,
        visited: &mut Vec<String>,
    ) -> Option<GpuUnsafe> {
        // Built-in heap collections — rejected outright regardless of args.
        if matches!(
            name,
            "Vec" | "VecDeque" | "Map" | "SortedMap" | "Set" | "SortedSet"
        ) {
            return Some(GpuUnsafe {
                leaf: name.to_string(),
                reason: GpuUnsafeReason::Heap,
                path: Vec::new(),
            });
        }

        // `Option[T]` / `Result[T, E]` — safe iff every type arg is safe.
        if matches!(name, "Option" | "Result") {
            return args
                .iter()
                .find_map(|a| self.gpu_unsafe_walk(a, subs, visited));
        }

        // User struct — recurse into fields with the generic params bound to
        // this instantiation's args. A `shared struct` carries `is_shared`
        // (its values are RC handles) and is rejected like `Type::Shared`.
        if let Some(info) = self.env.structs.get(name) {
            if info.is_shared {
                return Some(GpuUnsafe {
                    leaf: format!("shared {name}"),
                    reason: GpuUnsafeReason::SharedRc,
                    path: Vec::new(),
                });
            }
            if visited.iter().any(|v| v == name) {
                return None; // cycle guard (reached only via raw pointers)
            }
            visited.push(name.to_string());
            let field_subs = build_subs(&info.generic_params, args);
            let out = info.fields.iter().find_map(|(fname, fty, _)| {
                self.gpu_unsafe_walk(fty, &field_subs, visited)
                    .map(|mut bad| {
                        bad.path.insert(0, format!("{name}.{fname}"));
                        bad
                    })
            });
            visited.pop();
            return out;
        }

        // User enum — recurse into every variant payload.
        if let Some(info) = self.env.enums.get(name) {
            if info.is_shared {
                return Some(GpuUnsafe {
                    leaf: format!("shared {name}"),
                    reason: GpuUnsafeReason::SharedRc,
                    path: Vec::new(),
                });
            }
            if visited.iter().any(|v| v == name) {
                return None;
            }
            visited.push(name.to_string());
            let var_subs = build_subs(&info.generic_params, args);
            let out = info.variants.iter().find_map(|(vname, payload)| {
                let label = format!("{name}::{vname}");
                match payload {
                    VariantTypeInfo::Unit => None,
                    VariantTypeInfo::Tuple(types) => types
                        .iter()
                        .find_map(|t| self.payload_walk(t, &var_subs, visited, &label)),
                    VariantTypeInfo::Struct(fields) => fields.iter().find_map(|(fld, t)| {
                        self.payload_walk(t, &var_subs, visited, &format!("{label}.{fld}"))
                    }),
                }
            });
            visited.pop();
            return out;
        }

        // Unknown name (a generic param spelled as Named with no args, an
        // opaque/handle type, a distinct type, etc.) — defer to safe.
        None
    }

    fn payload_walk(
        &self,
        ty: &Type,
        subs: &HashMap<String, Type>,
        visited: &mut Vec<String>,
        label: &str,
    ) -> Option<GpuUnsafe> {
        self.gpu_unsafe_walk(ty, subs, visited).map(|mut bad| {
            bad.path.insert(0, label.to_string());
            bad
        })
    }
}

/// Build a generic-substitution map from a declaration's type-param names
/// zipped with the instantiation's concrete arguments. Extra params (when
/// `args` is shorter, e.g. defaults) are left unmapped → treated as safe.
fn build_subs(generic_params: &[String], args: &[Type]) -> HashMap<String, Type> {
    generic_params
        .iter()
        .zip(args.iter())
        .map(|(p, a)| (p.clone(), a.clone()))
        .collect()
}
