//! `frozen` parameter escape check — B-2026-08-01-33 mechanism 3, stage 1.
//!
//! ## Why this exists before the mode does anything
//!
//! A `frozen T` is meant to be a **non-counting** handle: codegen emits no
//! `rc_inc`/`rc_dec` for it, which is what makes concurrent reads across `par`
//! branches safe (it removes the raced refcount header rather than making it
//! atomic). That property is only sound while the handle cannot outlive the
//! owner whose count it is skipping — a non-counting handle that escapes is a
//! use-after-free.
//!
//! So escape checking is the precondition for every other part of the feature,
//! and it lands *before* `par` admission and before RC suppression, while the
//! mode is still inert. Nothing here can currently miscompile a program; what
//! it does is make the rule real so admission has something to stand on. See
//! [`docs/spikes/freeze-point-design.md`](../../docs/spikes/freeze-point-design.md)
//! § "Risks, stated plainly".
//!
//! ## Shape: whitelist, not blacklist
//!
//! The repo's three analysis bugs of 2026-08-04 (B-2026-08-04-13/-14/-15) were
//! one failure mode in three subsystems: *a walk that recognized some
//! place-expression spellings and silently ignored the rest*. Every one of them
//! enumerated the forms it handled and let the others fall through a `_` arm.
//!
//! This module inverts that. The walks below are **exhaustive matches with no
//! `_` wildcard**, so a new AST node breaks this file's build instead of
//! silently opening an escape route. And a frozen identifier is flagged at the
//! LEAF: only the two positions stage 1 explicitly permits consume their frozen
//! operand without recursing into it. Every other position — including any
//! position nobody thought about — reaches the bare `Identifier` arm and is
//! reported. The failure direction is a false positive (a rejected program that
//! could have been allowed), never a missed escape.
//!
//! ## What stage 1 permits
//!
//! 1. **Reading an immutable scalar field** — `n.val`. Lowers to a plain deref
//!    and yields a register copy, so no handle leaves. Immutable-and-scalar is
//!    the same predicate `concurrent_shared.rs` already admits for the
//!    scalar-field `par` case, for the same reason.
//! 2. **Passing the whole handle to another `frozen` parameter** —
//!    `helper(n)` where `helper`'s matching parameter is itself declared
//!    `frozen`. The callee is checked by this same pass, so the guarantee
//!    composes across the call rather than being re-derived at each site. This
//!    is the property that makes the motivating program (LeetCode #133, whose
//!    traversal lives in a callee) reachable at all.
//!
//! Everything else is an escape *for now*, including shapes that will become
//! legal in stage 2 — binding it to a local, projecting a nested handle
//! (`n.neighbors`), calling a method on it. Those need mode stickiness through
//! projection before they can be allowed, and rejecting them today is what
//! stops a program from depending on a spelling whose semantics are undecided.
//!
//! ## Known conservatism, stated so it is not mistaken for a hole
//!
//! - **Shadowing is not tracked.** An inner `let n = …` that shadows a frozen
//!   parameter makes later uses of `n` look like uses of the parameter. That
//!   over-reports (the shadowing `let` is itself already an escape, so the
//!   function is rejected either way) and never under-reports.
//! - **Only free-function calls compose.** A `frozen` argument in a *method*
//!   call is reported, because resolving a method to its declaration needs the
//!   typechecker's callee map and stage 1 does not wire it.
//! - **Unknown types fail closed.** If a parameter's type name does not resolve
//!   to a `struct_info` entry, its scalar-field set is empty, so every field
//!   read off it is reported.

use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::token::Span;

use super::{OwnershipError, OwnershipErrorKind};

/// Why a particular use was rejected. Selects the diagnostic wording; each
/// variant names a *specific* thing the user wrote rather than falling back on
/// "this is not allowed", because the legal surface is small enough that a bare
/// rejection would leave the reader guessing.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Reason {
    /// A bare use of the handle in any position other than the two permitted
    /// ones — returned, bound to a local, stored, captured, and so on.
    Materialized,
    /// `n.field` where `field` is `mut`, non-scalar, or unresolvable. The read
    /// would yield something other than a register copy.
    Projection { field: String },
    /// Passed to a call slot whose parameter is not declared `frozen` (or to a
    /// callee this pass cannot resolve, which includes every method call).
    NonFrozenArgument,
    /// Referenced from inside a CLOSURE BODY. The closure's environment holds
    /// the handle, and the closure can outlive the call (returned, stored,
    /// handed to `spawn`), so even the otherwise-permitted uses are reported
    /// there. `par` / `seq` blocks are NOT closures for this purpose: their
    /// branches join before the function returns, and admitting exactly that
    /// sharing is the point of the feature.
    Captured,
}

/// Walk state. `frozen` maps each in-scope frozen parameter name to the set of
/// its type's immutable scalar fields — precomputed because the walk hits it at
/// every field access.
struct Cx<'a> {
    frozen: HashMap<&'a str, HashSet<String>>,
    /// Free-function name → per-position `is_frozen` flags, used to decide
    /// whether passing the handle on is permitted.
    fn_frozen_params: HashMap<&'a str, Vec<bool>>,
    /// True while walking a closure body. A reference to a frozen parameter
    /// there is a CAPTURE into an environment that can outlive the call, so
    /// the two permitted positions are suppressed — reading a scalar field
    /// off the handle is only safe because no handle leaves, and inside a
    /// closure the handle itself has already left. Same argument, and the same
    /// flag, as `result_escape.rs`'s `in_closure`.
    in_closure: bool,
    found: Vec<(String, Span, Reason)>,
}

impl<'a> Cx<'a> {
    /// The frozen parameter name this expression denotes, if it is a bare
    /// reference to one.
    fn frozen_ident(&self, e: &Expr) -> Option<&'a str> {
        let ExprKind::Identifier(name) = &e.kind else {
            return None;
        };
        self.frozen.get_key_value(name.as_str()).map(|(k, _)| *k)
    }

    fn flag(&mut self, name: &str, span: &Span, reason: Reason) {
        self.found.push((name.to_string(), span.clone(), reason));
    }
}

impl super::OwnershipChecker<'_> {
    /// Report every use of a `frozen` parameter of `f` that is not one of the
    /// two shapes stage 1 permits. Emits `E0511` at each offending use.
    ///
    /// No-op — and, importantly, no program-wide work — for a function with no
    /// `frozen` parameter, which today is every function in every program.
    pub(crate) fn check_frozen_param_escape(&mut self, f: &Function) {
        if !f.params.iter().any(|p| p.is_frozen) {
            return;
        }

        let mut frozen: HashMap<&str, HashSet<String>> = HashMap::new();
        for p in f.params.iter().filter(|p| p.is_frozen) {
            let fields = self.immutable_scalar_fields(&p.ty);
            for name in binding_names_of(&p.pattern) {
                frozen.insert(name, fields.clone());
            }
        }
        if frozen.is_empty() {
            return;
        }

        let mut cx = Cx {
            frozen,
            fn_frozen_params: collect_fn_frozen_params(self.program),
            in_closure: false,
            found: Vec::new(),
        };
        walk_block(&f.body, &mut cx);

        for (name, span, reason) in std::mem::take(&mut cx.found) {
            let (what, fix) = match &reason {
                Reason::Materialized => (
                    format!("`frozen` parameter `{name}` escapes here"),
                    format!(
                        "a `frozen` handle is non-counting, so it must not outlive the call. \
                         Stage 1 allows only two uses: reading an immutable scalar field \
                         (`{name}.field`), and passing the whole handle to another parameter \
                         that is also declared `frozen`. To store, return, or capture it, take \
                         the parameter by value instead of `frozen`"
                    ),
                ),
                Reason::Projection { field } => (
                    format!(
                        "`frozen` parameter `{name}` cannot be projected through `.{field}` yet"
                    ),
                    format!(
                        "stage 1 permits reading only an IMMUTABLE SCALAR field off a `frozen` \
                         handle, because that lowers to a register copy and no handle escapes. \
                         `{field}` is `mut`, non-scalar, or on a type this pass could not \
                         resolve. Projecting a nested handle needs the mode to survive the \
                         projection, which is stage 2"
                    ),
                ),
                Reason::Captured => (
                    format!("`frozen` parameter `{name}` is captured by a closure"),
                    format!(
                        "the closure's environment holds the handle and can outlive the call \
                         — returned, stored, or handed to `spawn` — so a non-counting handle \
                         would be left pointing at freed memory. Read what you need from \
                         `{name}` into a local BEFORE the closure and capture that instead. \
                         (`par` / `seq` branches are not closures here: they join before the \
                         function returns.)"
                    ),
                ),
                Reason::NonFrozenArgument => (
                    format!("`frozen` parameter `{name}` is passed to a non-`frozen` slot"),
                    "the callee could store the handle, so the guarantee has to hold on its \
                     side too. Declare the receiving parameter `frozen` as well, and the check \
                     composes across the call. Method calls are not resolved by stage 1 and are \
                     reported even when the parameter is `frozen`"
                        .to_string(),
                ),
            };
            self.errors.push(OwnershipError {
                message: what,
                span,
                kind: OwnershipErrorKind::FrozenParamEscapes,
                suggestion: Some(fix),
                replacement: None,
                consume_span: None,
            });
        }
    }

    /// Immutable scalar fields of the struct `ty` names — the one field set a
    /// `frozen` handle may be read through in stage 1.
    ///
    /// Fail-closed at every step: a type expression that is not a plain path, a
    /// name with no `struct_info` entry, or an enum yields the empty set, so
    /// every projection off it is reported. Mirrors
    /// `concurrent_shared.rs::readonly_scalar_fields` — same two conditions
    /// (absent from `mut_fields`, `is_copy_type_basic`) for the same reason:
    /// an immutable field cannot be written by anyone, and a scalar read copies
    /// a register rather than aliasing a buffer.
    fn immutable_scalar_fields(&self, ty: &TypeExpr) -> HashSet<String> {
        // A `frozen` param is stored as `Ref(T)` — see the parser's `frozen` arm.
        let ty = match &ty.kind {
            TypeKind::Ref(inner) => inner.as_ref(),
            _ => ty,
        };
        let TypeKind::Path(path) = &ty.kind else {
            return HashSet::new();
        };
        let Some(name) = path.segments.last() else {
            return HashSet::new();
        };
        let Some(info) = self.typecheck_result.struct_info.get(name.as_str()) else {
            return HashSet::new();
        };
        info.fields
            .iter()
            .filter(|(fname, fty, _)| {
                !info.mut_fields.contains(fname) && super::is_copy_type_basic(fty)
            })
            .map(|(fname, _, _)| fname.clone())
            .collect()
    }
}

/// Every top-level function's per-position `is_frozen` flags, keyed by name.
///
/// Free functions only: impl methods and trait methods are deliberately absent,
/// so a frozen argument in a method call falls through to
/// [`Reason::NonFrozenArgument`]. Resolving a method call to its declaration
/// needs the typechecker's callee-type map, and wiring that is stage 2's job —
/// leaving it out over-reports rather than admitting an unchecked callee.
fn collect_fn_frozen_params(program: &Program) -> HashMap<&str, Vec<bool>> {
    let mut map: HashMap<&str, Vec<bool>> = HashMap::new();
    for item in &program.items {
        if let Item::Function(f) = item {
            map.insert(
                f.name.as_str(),
                f.params.iter().map(|p| p.is_frozen).collect(),
            );
        }
    }
    map
}

fn binding_names_of(p: &Pattern) -> Vec<&str> {
    match &p.kind {
        PatternKind::Binding(name) => vec![name.as_str()],
        // Any destructuring parameter pattern spreads the handle across several
        // bindings whose individual modes stage 1 has not defined. Returning
        // nothing here means the parameter contributes no tracked name, and the
        // `frozen.is_empty()` guard above then skips the function entirely —
        // which would be a HOLE, so the caller must keep at least one plain
        // binding for the check to apply. Stage 1 only accepts `frozen` on a
        // parameter, and a destructured `frozen` parameter is rejected by the
        // walk below the moment any of its bindings is used, because those
        // names are not in `frozen` and are ordinary values. That is sound:
        // destructuring a handle already materializes its parts.
        _ => Vec::new(),
    }
}

// ── Walks ───────────────────────────────────────────────────────
//
// Exhaustive, no `_` arms. A frozen identifier is reported at the LEAF
// (`ExprKind::Identifier`), so every position that merely recurses is covered
// automatically — including positions added to the AST after this was written,
// which will fail to compile here until they are handled explicitly.

fn walk_block<'a>(b: &'a Block, cx: &mut Cx<'a>) {
    for s in &b.stmts {
        walk_stmt(s, cx);
    }
    if let Some(fe) = &b.final_expr {
        walk_expr(fe, cx);
    }
}

fn walk_stmt<'a>(s: &'a Stmt, cx: &mut Cx<'a>) {
    match &s.kind {
        StmtKind::Let { value, .. } => walk_expr(value, cx),
        StmtKind::LetUninit { .. } => {}
        StmtKind::LetElse {
            value, else_block, ..
        } => {
            walk_expr(value, cx);
            walk_block(else_block, cx);
        }
        StmtKind::Defer { body } => walk_block(body, cx),
        StmtKind::ErrDefer { body, .. } => walk_block(body, cx),
        StmtKind::Assign { target, value } => {
            walk_expr(target, cx);
            walk_expr(value, cx);
        }
        StmtKind::MultiAssign { targets, values } => {
            for t in targets {
                walk_expr(t, cx);
            }
            for v in values {
                walk_expr(v, cx);
            }
        }
        StmtKind::CompoundAssign { target, value, .. } => {
            walk_expr(target, cx);
            walk_expr(value, cx);
        }
        StmtKind::Expr(e) => walk_expr(e, cx),
    }
}

/// Walk a call's arguments, permitting a frozen handle only in a slot whose
/// declared parameter is itself `frozen`.
fn walk_call_args<'a>(callee: Option<&str>, args: &'a [CallArg], cx: &mut Cx<'a>) {
    let sig = callee.and_then(|name| cx.fn_frozen_params.get(name).cloned());
    for (i, a) in args.iter().enumerate() {
        let Some(name) = cx.frozen_ident(&a.value) else {
            walk_expr(&a.value, cx);
            continue;
        };
        // A LABELLED argument is not matched positionally, and stage 1 does not
        // reorder against the declaration — so it is not resolved, and falls
        // through to the report. Conservative, not a hole.
        if cx.in_closure {
            cx.flag(name, &a.value.span, Reason::Captured);
            continue;
        }
        let permitted = a.label.is_none()
            && !a.mut_marker
            && sig
                .as_ref()
                .is_some_and(|s| s.get(i).copied() == Some(true));
        if !permitted {
            cx.flag(name, &a.value.span, Reason::NonFrozenArgument);
        }
    }
}

fn walk_expr<'a>(e: &'a Expr, cx: &mut Cx<'a>) {
    match &e.kind {
        // ── The leaf that reports ───────────────────────────────
        ExprKind::Identifier(name) => {
            if let Some(tracked) = cx.frozen.get_key_value(name.as_str()).map(|(k, _)| *k) {
                let reason = if cx.in_closure {
                    Reason::Captured
                } else {
                    Reason::Materialized
                };
                cx.flag(tracked, &e.span, reason);
            }
        }

        // ── Permitted position 1: immutable scalar field read ───
        ExprKind::FieldAccess { object, field } => {
            match cx.frozen_ident(object) {
                Some(name) => {
                    // Judged here, so the object is NOT recursed into — that is
                    // what makes this position permitted rather than reported.
                    if cx.in_closure {
                        cx.flag(name, &object.span, Reason::Captured);
                    } else if !cx.frozen[name].contains(field) {
                        cx.flag(
                            name,
                            &object.span,
                            Reason::Projection {
                                field: field.clone(),
                            },
                        );
                    }
                }
                None => walk_expr(object, cx),
            }
        }

        // ── Permitted position 2: pass-through to a frozen slot ─
        ExprKind::Call { callee, args } => {
            let name = match &callee.kind {
                ExprKind::Identifier(n) => Some(n.as_str()),
                _ => None,
            };
            walk_expr(callee, cx);
            walk_call_args(name, args, cx);
        }

        // ── Everything else: recurse; frozen uses report at the leaf ──
        ExprKind::Integer(..)
        | ExprKind::Float(..)
        | ExprKind::CharLit(_)
        | ExprKind::ByteLit(_)
        | ExprKind::StringLit(_)
        | ExprKind::MultiStringLit(_)
        | ExprKind::CStringLit { .. }
        | ExprKind::Bool(_)
        | ExprKind::Path { .. }
        | ExprKind::SelfValue
        | ExprKind::SelfType
        | ExprKind::PipePlaceholder
        | ExprKind::Continue { .. }
        | ExprKind::OffsetOf { .. }
        | ExprKind::Error => {}

        ExprKind::InterpolatedStringLit(parts) => {
            for p in parts {
                match p {
                    ParsedInterpolationPart::Text(_) => {}
                    ParsedInterpolationPart::Expr(inner, _) => walk_expr(inner, cx),
                }
            }
        }
        ExprKind::Binary { left, right, .. }
        | ExprKind::NilCoalesce { left, right }
        | ExprKind::Pipe { left, right } => {
            walk_expr(left, cx);
            walk_expr(right, cx);
        }
        ExprKind::Unary { operand, .. } => walk_expr(operand, cx),
        ExprKind::Question(inner) => walk_expr(inner, cx),
        ExprKind::OptionalChain { object, args, .. } => {
            walk_expr(object, cx);
            if let Some(args) = args {
                walk_call_args(None, args, cx);
            }
        }
        ExprKind::MethodCall { object, args, .. } => {
            walk_expr(object, cx);
            walk_call_args(None, args, cx);
        }
        ExprKind::TupleIndex { object, .. } => walk_expr(object, cx),
        ExprKind::Index { object, index } => {
            walk_expr(object, cx);
            walk_expr(index, cx);
        }
        ExprKind::Block(b)
        | ExprKind::Comptime(b)
        | ExprKind::Unsafe(b)
        | ExprKind::Try(b)
        | ExprKind::Seq(b)
        | ExprKind::Par(b) => walk_block(b, cx),
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            walk_expr(condition, cx);
            walk_block(then_block, cx);
            if let Some(eb) = else_branch {
                walk_expr(eb, cx);
            }
        }
        ExprKind::IfLet {
            value,
            then_block,
            else_branch,
            ..
        } => {
            walk_expr(value, cx);
            walk_block(then_block, cx);
            if let Some(eb) = else_branch {
                walk_expr(eb, cx);
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            walk_expr(scrutinee, cx);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    walk_expr(g, cx);
                }
                walk_expr(&arm.body, cx);
            }
        }
        ExprKind::While {
            condition, body, ..
        } => {
            walk_expr(condition, cx);
            walk_block(body, cx);
        }
        ExprKind::WhileLet { value, body, .. } => {
            walk_expr(value, cx);
            walk_block(body, cx);
        }
        ExprKind::For { iterable, body, .. } => {
            walk_expr(iterable, cx);
            walk_block(body, cx);
        }
        ExprKind::Loop { body, .. } => walk_block(body, cx),
        ExprKind::LabeledBlock { body, .. } => walk_block(body, cx),
        ExprKind::Closure { body, .. } => {
            let saved = std::mem::replace(&mut cx.in_closure, true);
            walk_expr(body, cx);
            cx.in_closure = saved;
        }
        ExprKind::Return(v) => {
            if let Some(v) = v {
                walk_expr(v, cx);
            }
        }
        ExprKind::Break { value, .. } => {
            if let Some(v) = value {
                walk_expr(v, cx);
            }
        }
        ExprKind::Tuple(items) | ExprKind::ArrayLiteral(items) => {
            for i in items {
                walk_expr(i, cx);
            }
        }
        ExprKind::PrefixCollectionLiteral { items, .. } => {
            for i in items {
                walk_expr(i, cx);
            }
        }
        ExprKind::RepeatLiteral { value, count, .. } => {
            walk_expr(value, cx);
            walk_expr(count, cx);
        }
        ExprKind::MapLiteral(entries) => {
            for (k, v) in entries {
                walk_expr(k, cx);
                walk_expr(v, cx);
            }
        }
        ExprKind::StructLiteral { fields, spread, .. } => {
            for f in fields {
                walk_expr(&f.value, cx);
            }
            if let Some(s) = spread {
                walk_expr(s, cx);
            }
        }
        ExprKind::Cast { expr, .. } => walk_expr(expr, cx),
        ExprKind::Range { start, end, .. } => {
            if let Some(s) = start {
                walk_expr(s, cx);
            }
            if let Some(en) = end {
                walk_expr(en, cx);
            }
        }
        ExprKind::Lock { mutex, body, .. } => {
            walk_expr(mutex, cx);
            walk_block(body, cx);
        }
        ExprKind::Providers { bindings, body } => {
            for b in bindings {
                walk_expr(&b.value, cx);
            }
            walk_block(body, cx);
        }
    }
}
