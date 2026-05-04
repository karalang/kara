//! Provider-rooted resource escape check.
//!
//! Rejects closures that capture a provider-rooted resource and flow out of
//! their `with_provider` / `providers { }` scope. See design.md §
//! Provider-Rooted Resources ("Provider-rooted resources cannot escape their
//! provider scope") and the CR-B entry in
//! `docs/implementation_checklist/phase-3-effect-checker.md`.
//!
//! Covered escape paths:
//! - Return-value escape (closure literal or let-bound identifier, including
//!   rebinding chains) via final expression or explicit `return`.
//! - Field / tuple-index / array-index assignment (`store.f = …`, `t.0 = …`,
//!   `arr[i] = …`).
//! - Outer-identifier reassignment (`x = closure` when `x` predates the
//!   innermost rooted frame).
//! - Instance-method call return / assignment (`v.method(…)`) via a
//!   `let_types` scope tracker; typecheck result threaded as fallback.
//! - Transitive capture through helper function and method calls via a
//!   program-wide `compute_escapable_caps` pre-pass.
//! - Channel-send escape (`Sender.send(closure)`) — receiver-type
//!   resolution via `let_types`, including the `let (tx, rx) = Channel.new()`
//!   destructuring pattern.
//! - Ambient program-rooted resources (`FileSystem`, `Clock`, `RandomSource`,
//!   `Env`, etc.) are exempt — only resources introduced by a `with_provider`
//!   / `providers { }` scope are pushed onto the rooted-stack.
//!
//! Deferred:
//! - `spawn` escape (`spawn(|| captures_rooted_resource)`) — blocked on
//!   `spawn(||…)` user syntax landing (Phase 6.3, deferred to v1.1 per
//!   `docs/roadmap.md`).

use crate::ast::*;
use crate::resolver::SpanKey;
use crate::token::Span;
use crate::typechecker::{Type, TypeCheckResult};
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct EscapeError {
    /// Rooted resource name captured by the closure.
    pub resource: String,
    /// Span of the `with_provider[R]` call or `providers { }` block that
    /// roots the resource. Drives the "rooted here" anchor in diagnostics.
    pub provider_span: Span,
    /// Span of the escaping closure literal.
    pub closure_span: Span,
    /// Span of the escape path — the `return` keyword or the block-final
    /// expression that carries the closure out.
    pub escape_span: Span,
    /// Category of escape path taken. Drives the diagnostic suffix.
    pub kind: EscapeKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EscapeKind {
    /// Closure appears as the final expression of a `with_provider` body
    /// closure or `providers { } in { body }` block — its value flows back
    /// to the block's caller.
    BlockFinalValue,
    /// Closure appears in a `return <expr>` inside a `with_provider` body
    /// closure.
    ReturnValue,
    /// Closure is assigned to a field (`foo.bar = ...`) or indexed slot
    /// (`arr[i] = ...`) inside a rooted scope. Conservative: any field /
    /// index target is assumed to outlive the provider scope. Local
    /// bindings declared inside the same scope trigger false positives
    /// here — users can restructure or add a dedicated escape hatch if
    /// the pattern proves common.
    FieldAssignment {
        /// Dotted or bracketed target as rendered for the message, e.g.
        /// `store.f` or `cache[0]`. Best-effort string for diagnostics.
        target_desc: String,
    },
    /// Closure is assigned to a plain identifier whose let-binding sits
    /// in a scope that was already open when the innermost `with_provider`
    /// frame was pushed — i.e., the identifier outlives the provider
    /// scope. Identifiers declared *inside* the rooted scope are not
    /// flagged; they're dropped along with the scope.
    OuterIdentifierAssignment {
        /// The identifier whose value was overwritten.
        target_name: String,
    },
    /// Closure is sent across a channel via `Sender.send(closure)`. The
    /// receiving end may be on another thread or outlive the provider
    /// scope, so anything sent is treated as escaping. Cloned senders
    /// (`tx.clone().send(...)`) propagate the rule because the receiver
    /// type is still `Sender[T]`.
    ChannelSend,
}

impl EscapeError {
    pub fn message(&self) -> String {
        let path = match &self.kind {
            EscapeKind::BlockFinalValue => "value returned as block's final expression".to_string(),
            EscapeKind::ReturnValue => "value returned via `return`".to_string(),
            EscapeKind::FieldAssignment { target_desc } => {
                format!("value assigned to `{}`", target_desc)
            }
            EscapeKind::OuterIdentifierAssignment { target_name } => {
                format!("value reassigned to outer binding `{}`", target_name)
            }
            EscapeKind::ChannelSend => "value sent via channel".to_string(),
        };
        format!(
            "closure captures provider-rooted resource `{}` but escapes its provider scope ({})",
            self.resource, path
        )
    }
}

/// Run the escape check over `program`. Returns every violation found.
///
/// When `types` is `Some`, instance-method calls (`v.method(...)`) are
/// resolved to `TypeName.method` via `expr_types`; when `None`, instance
/// methods are conservatively skipped (they may still escape via transitive
/// capture through other paths).
pub fn check_provider_escape(
    program: &Program,
    types: Option<&TypeCheckResult>,
) -> Vec<EscapeError> {
    let escapable_caps = compute_escapable_caps(program);
    let mut checker = EscapeChecker {
        errors: Vec::new(),
        rooted: Vec::new(),
        closure_bindings: Vec::new(),
        escapable_caps,
        types,
    };
    for item in &program.items {
        match item {
            Item::Function(f) => checker.visit_function(&f.params, &f.body),
            Item::ImplBlock(imp) => {
                for i in &imp.items {
                    if let ImplItem::Method(m) = i {
                        checker.visit_function(&m.params, &m.body);
                    }
                }
            }
            _ => {}
        }
    }
    checker.errors
}

/// Pre-pass: for each top-level function and impl method, compute the set
/// of resource names that appear inside *closure literals* in its body.
/// Intuitively: "if a caller captures the return value of this function
/// (or a transitive callee's return value) at an escape position, which
/// rooted resources might leak?" The result is an overapproximation — a
/// function that reads `Clock` inline without ever returning a closure
/// still appears in this map — but the escape site filters down to only
/// resources currently on the rooted stack, so non-rooted reads never
/// produce errors.
///
/// Keys follow the effect checker's method naming: free functions by
/// their bare name, impl methods as `"TypeName.method_name"`.
fn compute_escapable_caps(program: &Program) -> HashMap<String, Vec<String>> {
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    for item in &program.items {
        match item {
            Item::Function(f) => {
                let mut caps = Vec::new();
                collect_closure_resource_refs_in_block(&f.body, &mut caps);
                out.insert(f.name.clone(), caps);
            }
            Item::ImplBlock(imp) => {
                let type_name = match &imp.target_type.kind {
                    TypeKind::Path(p) if !p.segments.is_empty() => p.segments.last().cloned(),
                    _ => None,
                };
                let Some(type_name) = type_name else { continue };
                for i in &imp.items {
                    if let ImplItem::Method(m) = i {
                        let mut caps = Vec::new();
                        collect_closure_resource_refs_in_block(&m.body, &mut caps);
                        out.insert(format!("{}.{}", type_name, m.name), caps);
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Walk a block collecting resource names that appear *only* inside
/// closure literals. A resource read at top level of the body doesn't
/// count — it's consumed inline and cannot leak via the return value.
/// This mirrors the "did this function's body bake a rooted-capturing
/// closure" question the escape check asks at call sites.
fn collect_closure_resource_refs_in_block(block: &Block, out: &mut Vec<String>) {
    for stmt in &block.stmts {
        match &stmt.kind {
            StmtKind::Let { value, .. } => collect_closure_resource_refs_in_expr(value, out),
            StmtKind::LetUninit { .. } => {}
            StmtKind::LetElse {
                value, else_block, ..
            } => {
                collect_closure_resource_refs_in_expr(value, out);
                collect_closure_resource_refs_in_block(else_block, out);
            }
            StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
                collect_closure_resource_refs_in_expr(target, out);
                collect_closure_resource_refs_in_expr(value, out);
            }
            StmtKind::Expr(e) => collect_closure_resource_refs_in_expr(e, out),
            StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                collect_closure_resource_refs_in_block(body, out);
            }
        }
    }
    if let Some(e) = &block.final_expr {
        collect_closure_resource_refs_in_expr(e, out);
    }
}

fn collect_closure_resource_refs_in_expr(expr: &Expr, out: &mut Vec<String>) {
    match &expr.kind {
        ExprKind::Closure { body, .. } => {
            // Every resource referenced inside a closure literal is
            // capturable — that's the whole point of this pass.
            collect_resource_refs(body, out);
        }
        ExprKind::Binary { left, right, .. } | ExprKind::Pipe { left, right } => {
            collect_closure_resource_refs_in_expr(left, out);
            collect_closure_resource_refs_in_expr(right, out);
        }
        ExprKind::NilCoalesce { left, right } => {
            collect_closure_resource_refs_in_expr(left, out);
            collect_closure_resource_refs_in_expr(right, out);
        }
        ExprKind::Unary { operand, .. } | ExprKind::Question(operand) => {
            collect_closure_resource_refs_in_expr(operand, out);
        }
        ExprKind::Call { callee, args } => {
            collect_closure_resource_refs_in_expr(callee, out);
            for a in args {
                collect_closure_resource_refs_in_expr(&a.value, out);
            }
        }
        ExprKind::MethodCall { object, args, .. } => {
            collect_closure_resource_refs_in_expr(object, out);
            for a in args {
                collect_closure_resource_refs_in_expr(&a.value, out);
            }
        }
        ExprKind::OptionalChain { object, args, .. } => {
            collect_closure_resource_refs_in_expr(object, out);
            if let Some(args) = args {
                for a in args {
                    collect_closure_resource_refs_in_expr(&a.value, out);
                }
            }
        }
        ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
            collect_closure_resource_refs_in_expr(object, out);
        }
        ExprKind::Index { object, index } => {
            collect_closure_resource_refs_in_expr(object, out);
            collect_closure_resource_refs_in_expr(index, out);
        }
        ExprKind::Block(b)
        | ExprKind::Unsafe(b)
        | ExprKind::Seq(b)
        | ExprKind::Par(b)
        | ExprKind::Lock { body: b, .. } => collect_closure_resource_refs_in_block(b, out),
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            collect_closure_resource_refs_in_expr(condition, out);
            collect_closure_resource_refs_in_block(then_block, out);
            if let Some(eb) = else_branch {
                collect_closure_resource_refs_in_expr(eb, out);
            }
        }
        ExprKind::IfLet {
            value,
            then_block,
            else_branch,
            ..
        } => {
            collect_closure_resource_refs_in_expr(value, out);
            collect_closure_resource_refs_in_block(then_block, out);
            if let Some(eb) = else_branch {
                collect_closure_resource_refs_in_expr(eb, out);
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            collect_closure_resource_refs_in_expr(scrutinee, out);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    collect_closure_resource_refs_in_expr(g, out);
                }
                collect_closure_resource_refs_in_expr(&arm.body, out);
            }
        }
        ExprKind::While {
            condition, body, ..
        } => {
            collect_closure_resource_refs_in_expr(condition, out);
            collect_closure_resource_refs_in_block(body, out);
        }
        ExprKind::WhileLet { value, body, .. } => {
            collect_closure_resource_refs_in_expr(value, out);
            collect_closure_resource_refs_in_block(body, out);
        }
        ExprKind::For { iterable, body, .. } => {
            collect_closure_resource_refs_in_expr(iterable, out);
            collect_closure_resource_refs_in_block(body, out);
        }
        ExprKind::Loop { body, .. } => collect_closure_resource_refs_in_block(body, out),
        ExprKind::Return(Some(inner))
        | ExprKind::Break {
            value: Some(inner), ..
        } => collect_closure_resource_refs_in_expr(inner, out),
        ExprKind::Tuple(exprs) | ExprKind::ArrayLiteral(exprs) => {
            for e in exprs {
                collect_closure_resource_refs_in_expr(e, out);
            }
        }
        ExprKind::RepeatLiteral { value, count, .. } => {
            collect_closure_resource_refs_in_expr(value, out);
            collect_closure_resource_refs_in_expr(count, out);
        }
        ExprKind::MapLiteral(entries) => {
            for (k, v) in entries {
                collect_closure_resource_refs_in_expr(k, out);
                collect_closure_resource_refs_in_expr(v, out);
            }
        }
        ExprKind::StructLiteral { fields, spread, .. } => {
            for f in fields {
                collect_closure_resource_refs_in_expr(&f.value, out);
            }
            if let Some(s) = spread {
                collect_closure_resource_refs_in_expr(s, out);
            }
        }
        ExprKind::Cast { expr: inner, .. } => collect_closure_resource_refs_in_expr(inner, out),
        ExprKind::Range { start, end, .. } => {
            if let Some(s) = start {
                collect_closure_resource_refs_in_expr(s, out);
            }
            if let Some(e) = end {
                collect_closure_resource_refs_in_expr(e, out);
            }
        }
        ExprKind::Providers { bindings, body } => {
            for b in bindings {
                collect_closure_resource_refs_in_expr(&b.value, out);
            }
            collect_closure_resource_refs_in_block(body, out);
        }
        _ => {}
    }
}

/// One frame of the rooted-resource stack. Each `with_provider` / `providers`
/// scope pushes a frame; exit pops it. A closure literal whose body
/// references a resource name bound in *any* live frame is a capture.
struct RootedScope {
    /// Resource names rooted here → span of their introduction site.
    /// Multiple for `providers { ... }`; exactly one for `with_provider`.
    resources: Vec<(String, Span)>,
    /// Span of the `with_provider` call or `providers` block as a whole,
    /// used as the "rooted here" anchor in the error message.
    block_span: Span,
}

struct EscapeChecker<'a> {
    errors: Vec<EscapeError>,
    rooted: Vec<RootedScope>,
    /// Scope stack mirroring block nesting. Each frame records every
    /// `let`-bound name declared in that scope (so we can distinguish
    /// inner-scope from outer-scope identifiers at assignment sites) and
    /// the `rooted.len()` at the time the scope was pushed (so an
    /// assignment to a name whose scope predates the innermost rooted
    /// frame can be flagged as an escape). `closures` holds the subset
    /// whose value is a closure literal, paired with its directly-
    /// referenced rooted resources — used to resolve `return x` / block-
    /// final `x` when `x` is an identifier.
    closure_bindings: Vec<ClosureScope>,
    /// Program-wide map of function/method name → resource names that
    /// could leak if the function's return value reaches an escape
    /// position. See `compute_escapable_caps` for the overapproximation.
    escapable_caps: HashMap<String, Vec<String>>,
    /// Optional typecheck result for resolving instance-method receivers.
    /// When present, `v.method(...)` is dispatched to `TypeName.method` in
    /// `escapable_caps` via `expr_types[&v.span]`.
    types: Option<&'a TypeCheckResult>,
}

/// One frame of the `closure_bindings` stack. Mirrors a lexical block's
/// scope — pushed on block entry, popped on block exit.
struct ClosureScope {
    /// Every `let`-bound name declared in this scope, regardless of the
    /// bound value's shape. Used so plain identifier assignment targets
    /// can be classified as inner-scope (same or deeper rooted frame)
    /// vs outer-scope (predates the innermost rooted frame).
    all_names: std::collections::HashSet<String>,
    /// Subset of `all_names` whose let value is — transitively via
    /// rebinding chains — a closure literal. Stores the rooted-resource
    /// refs the closure captures at its definition site.
    closures: HashMap<String, Vec<String>>,
    /// Subset of `all_names` whose inferred type name can be recovered
    /// structurally (from a struct literal, a `T.new(...)` call, or a
    /// parameter type annotation). Used to resolve `v.method(...)` to
    /// `TypeName.method` for the escapable-caps lookup. See
    /// `extract_type_name_from_let_value` for recognized shapes.
    let_types: HashMap<String, String>,
    /// `rooted.len()` at the moment this scope was pushed. A name
    /// declared here is "outer" relative to any innermost rooted frame
    /// opened *after* `rooted_at_push`.
    rooted_at_push: usize,
}

impl<'a> EscapeChecker<'a> {
    /// Walk a block. `in_escape` is true when the block's *value* flows out
    /// as an escape path (e.g., it's the body of a `with_provider` closure
    /// and we want its final expression / explicit returns to be checked).
    fn visit_block(&mut self, block: &Block, in_escape: bool) {
        self.push_scope();
        for stmt in &block.stmts {
            self.visit_stmt(stmt, in_escape);
        }
        if let Some(e) = &block.final_expr {
            // Final expression of the block carries the block's value.
            if in_escape {
                self.check_escape_expr(e, EscapeKind::BlockFinalValue, &e.span);
            }
            self.visit_expr(e, in_escape);
        }
        self.closure_bindings.pop();
    }

    /// Walk a function (or method) body and seed parameter types into the
    /// body's top-level scope so `v.method(...)` on a parameter can be
    /// resolved the same way as a `let`-bound instance. Opening the scope
    /// here — rather than letting `visit_block` do it — is what lets
    /// parameter types be visible inside the body.
    fn visit_function(&mut self, params: &[Param], body: &Block) {
        self.push_scope();
        for p in params {
            if let Some(name) = p.name() {
                self.note_let_binding(name.to_string());
                if let Some(ty_name) = type_expr_name(&p.ty) {
                    self.note_let_type(name.to_string(), ty_name);
                }
            }
        }
        for stmt in &body.stmts {
            self.visit_stmt(stmt, false);
        }
        if let Some(e) = &body.final_expr {
            self.visit_expr(e, false);
        }
        self.closure_bindings.pop();
    }

    fn visit_stmt(&mut self, stmt: &Stmt, in_escape: bool) {
        match &stmt.kind {
            StmtKind::Let { pattern, value, .. } => {
                self.visit_expr(value, false);
                // Record the binding. Simple name patterns are tracked in
                // `all_names` regardless of value shape (so assignment
                // targets can be classified later); closure values also
                // populate `closures` with their rooted-resource refs.
                // Destructuring patterns are not tracked in this slice —
                // they don't come up in typical escape patterns and add
                // complexity for limited gain.
                if let PatternKind::Binding(name) = &pattern.kind {
                    self.note_let_binding(name.clone());
                    if let Some(refs) = self.closure_refs_of(value) {
                        self.bind_closure(name.clone(), refs);
                    }
                    if let Some(ty_name) = extract_type_name_from_let_value(value) {
                        self.note_let_type(name.clone(), ty_name);
                    }
                }
                // `let (tx, rx) = Channel.new()` — record the destructured
                // sender/receiver bindings so `tx.send(...)` resolves to
                // the `Sender` type for channel-send escape detection.
                if let PatternKind::Tuple(elems) = &pattern.kind {
                    self.note_channel_tuple_destructure(elems, value);
                }
            }
            StmtKind::LetElse {
                pattern,
                value,
                else_block,
                ..
            } => {
                self.visit_expr(value, false);
                self.visit_block(else_block, false);
                if let PatternKind::Binding(name) = &pattern.kind {
                    self.note_let_binding(name.clone());
                    if let Some(refs) = self.closure_refs_of(value) {
                        self.bind_closure(name.clone(), refs);
                    }
                    if let Some(ty_name) = extract_type_name_from_let_value(value) {
                        self.note_let_type(name.clone(), ty_name);
                    }
                }
            }
            StmtKind::LetUninit { name, .. } => {
                // Record the binding so subsequent assignments classify
                // correctly. No closure / type tracking — there's no RHS yet.
                self.note_let_binding(name.clone());
            }
            StmtKind::Assign { target, value } => {
                self.visit_expr(target, false);
                if !self.rooted.is_empty() {
                    if is_field_like_target(target) {
                        // Field / index / tuple-index assignment —
                        // conservatively treat any such target as
                        // outliving the provider block.
                        let desc = render_assign_target(target);
                        self.check_field_assign_escape(value, &desc, &target.span);
                    } else if let ExprKind::Identifier(name) = &target.kind {
                        // Plain identifier — only a problem if the name's
                        // let-binding predates the innermost rooted frame.
                        // Inner-scope reassignments (`let mut c = |...|;
                        // c = ...;`) drop with the scope and are fine.
                        if self.is_outer_binding(name) {
                            self.check_outer_identifier_escape(value, name, &target.span);
                        }
                    }
                }
                self.visit_expr(value, false);
            }
            StmtKind::CompoundAssign { target, value, .. } => {
                self.visit_expr(target, false);
                self.visit_expr(value, false);
            }
            StmtKind::Expr(e) => self.visit_expr(e, in_escape),
            StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                self.visit_block(body, false);
            }
        }
    }

    fn push_scope(&mut self) {
        self.closure_bindings.push(ClosureScope {
            all_names: std::collections::HashSet::new(),
            closures: HashMap::new(),
            let_types: HashMap::new(),
            rooted_at_push: self.rooted.len(),
        });
    }

    fn note_let_binding(&mut self, name: String) {
        if let Some(top) = self.closure_bindings.last_mut() {
            top.all_names.insert(name);
        }
    }

    fn note_let_type(&mut self, name: String, type_name: String) {
        if let Some(top) = self.closure_bindings.last_mut() {
            top.let_types.insert(name, type_name);
        }
    }

    /// Look up a let-binding's structurally-recovered type name. Walks the
    /// scope stack innermost-first.
    fn lookup_let_type(&self, name: &str) -> Option<&str> {
        for scope in self.closure_bindings.iter().rev() {
            if let Some(ty) = scope.let_types.get(name) {
                return Some(ty.as_str());
            }
        }
        None
    }

    fn bind_closure(&mut self, name: String, refs: Vec<String>) {
        if let Some(top) = self.closure_bindings.last_mut() {
            top.closures.insert(name, refs);
        }
    }

    fn lookup_closure_binding(&self, name: &str) -> Option<&Vec<String>> {
        for scope in self.closure_bindings.iter().rev() {
            if let Some(refs) = scope.closures.get(name) {
                return Some(refs);
            }
        }
        None
    }

    /// True iff `name` is bound in some scope that was already open when
    /// the innermost rooted frame was pushed. Unbound names (e.g., outer
    /// function params, module-level items) also count as outer — their
    /// lifetime by definition predates the provider scope.
    fn is_outer_binding(&self, name: &str) -> bool {
        for scope in self.closure_bindings.iter().rev() {
            if scope.all_names.contains(name) {
                return scope.rooted_at_push < self.rooted.len();
            }
        }
        // No binding found in any tracked scope — treat as outer to be
        // safe (parameters, module-level functions, let-bound outside
        // any tracked walk).
        true
    }

    /// If `value` is a closure literal, return the rooted resources it
    /// references directly. If it's an identifier that already resolves to
    /// a tracked closure, return its recorded refs. Otherwise `None` —
    /// the binding is not tracked.
    fn closure_refs_of(&self, value: &Expr) -> Option<Vec<String>> {
        match &value.kind {
            ExprKind::Closure { body, .. } => {
                let mut refs = Vec::new();
                collect_resource_refs(body, &mut refs);
                Some(refs)
            }
            ExprKind::Identifier(name) => self.lookup_closure_binding(name).cloned(),
            _ => None,
        }
    }

    fn visit_expr(&mut self, expr: &Expr, in_escape: bool) {
        // Intercept `return <expr>` — its operand is in an escape position.
        if let ExprKind::Return(Some(inner)) = &expr.kind {
            self.check_escape_expr(inner, EscapeKind::ReturnValue, &expr.span);
            self.visit_expr(inner, false);
            return;
        }

        // Recognize `with_provider[R](provider, closure)` shape and, if it
        // matches, descend into the closure body. The body's return value
        // always flows back to `with_provider`'s caller — which counts as
        // escape for rooted-resource purposes, regardless of whether the
        // outer context uses the return value. The check is conservative
        // ("may escape"): we do not follow the caller to decide whether the
        // value is dropped; any closure appearing at the body's return
        // position is treated as escaping.
        if let ExprKind::Call { callee, args } = &expr.kind {
            if let Some((resource, provider_expr, closure_expr)) = match_with_provider(callee, args)
            {
                self.visit_expr(provider_expr, false);
                self.enter_with_provider(resource, expr.span.clone());
                self.visit_closure_body(closure_expr, true);
                self.exit_scope();
                return;
            }
        }

        // `providers { R1 => e1, ... } in { body }` — each binding is a
        // plain subexpression; the body's return value flows out of the
        // block, so enter it with `in_escape=true` regardless of the outer
        // context. Same "may escape" reasoning as `with_provider`.
        if let ExprKind::Providers { bindings, body } = &expr.kind {
            for b in bindings {
                self.visit_expr(&b.value, false);
            }
            self.enter_providers(bindings, expr.span.clone());
            self.visit_block(body, true);
            self.exit_scope();
            return;
        }

        // Default recursive walk.
        match &expr.kind {
            ExprKind::Binary { left, right, .. } | ExprKind::Pipe { left, right } => {
                self.visit_expr(left, false);
                self.visit_expr(right, false);
            }
            ExprKind::NilCoalesce { left, right } => {
                self.visit_expr(left, false);
                self.visit_expr(right, false);
            }
            ExprKind::Unary { operand, .. } | ExprKind::Question(operand) => {
                self.visit_expr(operand, false);
            }
            ExprKind::Call { callee, args } => {
                self.visit_expr(callee, false);
                for a in args {
                    self.visit_expr(&a.value, false);
                }
            }
            ExprKind::MethodCall {
                object,
                method,
                args,
                ..
            } => {
                self.check_channel_send_escape(object, method, args, &expr.span);
                self.visit_expr(object, false);
                for a in args {
                    self.visit_expr(&a.value, false);
                }
            }
            ExprKind::OptionalChain { object, args, .. } => {
                self.visit_expr(object, false);
                if let Some(args) = args {
                    for a in args {
                        self.visit_expr(&a.value, false);
                    }
                }
            }
            ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
                self.visit_expr(object, false);
            }
            ExprKind::Index { object, index } => {
                self.visit_expr(object, false);
                self.visit_expr(index, false);
            }
            ExprKind::Block(b)
            | ExprKind::Unsafe(b)
            | ExprKind::Seq(b)
            | ExprKind::Par(b)
            | ExprKind::Lock { body: b, .. } => {
                self.visit_block(b, in_escape);
            }
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                self.visit_expr(condition, false);
                self.visit_block(then_block, in_escape);
                if let Some(eb) = else_branch {
                    self.visit_expr(eb, in_escape);
                }
            }
            ExprKind::IfLet {
                value,
                then_block,
                else_branch,
                ..
            } => {
                self.visit_expr(value, false);
                self.visit_block(then_block, in_escape);
                if let Some(eb) = else_branch {
                    self.visit_expr(eb, in_escape);
                }
            }
            ExprKind::Match { scrutinee, arms } => {
                self.visit_expr(scrutinee, false);
                for arm in arms {
                    if let Some(g) = &arm.guard {
                        self.visit_expr(g, false);
                    }
                    // Each arm body carries the match's value.
                    self.visit_expr(&arm.body, in_escape);
                }
            }
            ExprKind::While {
                condition, body, ..
            } => {
                self.visit_expr(condition, false);
                self.visit_block(body, false);
            }
            ExprKind::WhileLet { value, body, .. } => {
                self.visit_expr(value, false);
                self.visit_block(body, false);
            }
            ExprKind::For { iterable, body, .. } => {
                self.visit_expr(iterable, false);
                self.visit_block(body, false);
            }
            ExprKind::Loop { body, .. } => {
                self.visit_block(body, false);
            }
            ExprKind::Closure { body, .. } => {
                // A closure literal that is *not* at an escape position
                // still needs to be walked for nested `with_provider`
                // blocks. The closure body opens a fresh function scope —
                // we suppress the caller's rooted-stack since the closure
                // may be invoked in a different context. Entering it with
                // an empty stack is the conservative choice: captures from
                // the outer rooted scope are handled at the closure-literal
                // site, not inside the body.
                let saved = std::mem::take(&mut self.rooted);
                self.visit_expr(body, false);
                self.rooted = saved;
            }
            ExprKind::Return(None) => {}
            ExprKind::Break {
                value: Some(inner), ..
            } => {
                self.visit_expr(inner, false);
            }
            ExprKind::Tuple(exprs) | ExprKind::ArrayLiteral(exprs) => {
                for e in exprs {
                    self.visit_expr(e, false);
                }
            }
            ExprKind::RepeatLiteral { value, count, .. } => {
                self.visit_expr(value, false);
                self.visit_expr(count, false);
            }
            ExprKind::MapLiteral(entries) => {
                for (k, v) in entries {
                    self.visit_expr(k, false);
                    self.visit_expr(v, false);
                }
            }
            ExprKind::StructLiteral { fields, spread, .. } => {
                for f in fields {
                    self.visit_expr(&f.value, false);
                }
                if let Some(s) = spread {
                    self.visit_expr(s, false);
                }
            }
            ExprKind::Cast { expr: inner, .. } => {
                self.visit_expr(inner, false);
            }
            ExprKind::Range { start, end, .. } => {
                if let Some(s) = start {
                    self.visit_expr(s, false);
                }
                if let Some(e) = end {
                    self.visit_expr(e, false);
                }
            }
            // Leaves — nothing to recurse into.
            _ => {}
        }
    }

    /// Visit the closure argument of `with_provider[R](p, closure)`. The
    /// typical shape is a zero-arg closure literal `|| { ... }`; we push
    /// through to its body so the body's final expression inherits the
    /// outer `in_escape` context (the body's value is exactly what
    /// `with_provider` returns).
    fn visit_closure_body(&mut self, closure_expr: &Expr, in_escape: bool) {
        match &closure_expr.kind {
            ExprKind::Closure { body, .. } => {
                self.visit_expr(body, in_escape);
            }
            _ => self.visit_expr(closure_expr, in_escape),
        }
    }

    fn enter_with_provider(&mut self, resource: String, block_span: Span) {
        self.rooted.push(RootedScope {
            resources: vec![(resource, block_span.clone())],
            block_span,
        });
    }

    fn enter_providers(&mut self, bindings: &[ProviderBinding], block_span: Span) {
        let resources = bindings
            .iter()
            .map(|b| (b.resource.clone(), b.resource_span.clone()))
            .collect();
        self.rooted.push(RootedScope {
            resources,
            block_span,
        });
    }

    fn exit_scope(&mut self) {
        self.rooted.pop();
    }

    /// If `expr` resolves to a closure — either a literal at the escape
    /// site, a tracked `let`-bound name pointing at one, or a call to a
    /// function whose body builds a closure capturing a rooted resource —
    /// push an error for each rooted resource leaked. The caller has
    /// already descended into `expr`, so no further walking happens here.
    fn check_escape_expr(&mut self, expr: &Expr, kind: EscapeKind, escape_span: &Span) {
        if self.rooted.is_empty() {
            return;
        }
        let used_resources: Vec<String> = match &expr.kind {
            ExprKind::Closure { body, .. } => {
                let mut out = Vec::new();
                collect_resource_refs(body, &mut out);
                out
            }
            ExprKind::Identifier(name) => match self.lookup_closure_binding(name) {
                Some(refs) => refs.clone(),
                None => return,
            },
            ExprKind::Call { callee, .. } => match self.call_escapable_caps(callee) {
                Some(refs) if !refs.is_empty() => refs,
                _ => return,
            },
            ExprKind::MethodCall { object, method, .. } => {
                // Resolve the receiver to a type name, then look up
                // `"TypeName.method"` in the escapable-caps map.
                // - `T.method(...)` with a bare type identifier resolves
                //   directly.
                // - `v.method(...)` (instance call) consults the typecheck
                //   result when available and strips `ref` / `mut ref` /
                //   `&` wrappers to find the underlying `Named` type name.
                let ty_name = self.resolve_receiver_type_name(object);
                let Some(ty_name) = ty_name else { return };
                let key = format!("{}.{}", ty_name, method);
                match self.escapable_caps.get(&key) {
                    Some(refs) if !refs.is_empty() => refs.clone(),
                    _ => return,
                }
            }
            _ => return,
        };
        for (resource, res_span) in self.all_rooted() {
            if used_resources.iter().any(|r| r == &resource) {
                self.errors.push(EscapeError {
                    resource,
                    provider_span: res_span,
                    closure_span: expr.span.clone(),
                    escape_span: escape_span.clone(),
                    kind: kind.clone(),
                });
            }
        }
    }

    /// Resolve a `Call { callee, ... }` callee to a key in
    /// `escapable_caps` and return the cap list. Accepts bare identifiers
    /// (free functions) and two-segment paths (`TypeName.method` from
    /// `parse_primary`'s capital-leading-ident treatment).
    fn call_escapable_caps(&self, callee: &Expr) -> Option<Vec<String>> {
        let key = match &callee.kind {
            ExprKind::Identifier(name) => name.clone(),
            ExprKind::Path(segs) if segs.len() == 2 => format!("{}.{}", segs[0], segs[1]),
            _ => return None,
        };
        self.escapable_caps.get(&key).cloned()
    }

    /// Recognize `let (tx, rx) = Channel.new()` and seed the bindings'
    /// types into `let_types` as `Sender` / `Receiver`. Needed because
    /// the general destructure path doesn't track binding types, and
    /// `Channel.new()` is the idiomatic way to obtain a `Sender[T]` for
    /// a `.send()` call. Conservatively requires the value to be exactly
    /// `Channel.new()` (no args, two-name tuple); other tuple shapes
    /// fall through unrecorded.
    fn note_channel_tuple_destructure(&mut self, elems: &[Pattern], value: &Expr) {
        if elems.len() != 2 {
            return;
        }
        let (PatternKind::Binding(tx), PatternKind::Binding(rx)) = (&elems[0].kind, &elems[1].kind)
        else {
            return;
        };
        let is_channel_new = match &value.kind {
            ExprKind::MethodCall { object, method, .. } => {
                method == "new" && matches!(&object.kind, ExprKind::Identifier(n) if n == "Channel")
            }
            ExprKind::Call { callee, .. } => {
                matches!(&callee.kind, ExprKind::Path(segs)
                    if segs.len() == 2 && segs[0] == "Channel" && segs[1] == "new")
            }
            _ => false,
        };
        if !is_channel_new {
            return;
        }
        self.note_let_type(tx.clone(), "Sender".to_string());
        self.note_let_type(rx.clone(), "Receiver".to_string());
    }

    /// Detect `Sender.send(closure)` calls inside a rooted scope and
    /// dispatch the escape check on the argument. Channel-send is treated
    /// as a hard escape boundary: anything sent may flow to a thread or
    /// continuation that outlives the provider scope. Cloned senders
    /// (`tx.clone().send(...)`) propagate the rule because the receiver
    /// type stays `Sender[T]`.
    fn check_channel_send_escape(
        &mut self,
        object: &Expr,
        method: &str,
        args: &[CallArg],
        call_span: &Span,
    ) {
        if method != "send" || self.rooted.is_empty() || args.is_empty() {
            return;
        }
        let Some(ty_name) = self.resolve_receiver_type_name(object) else {
            return;
        };
        if ty_name != "Sender" {
            return;
        }
        self.check_escape_expr(&args[0].value, EscapeKind::ChannelSend, call_span);
    }

    /// Resolve the receiver of a `MethodCall` to a user-defined type name
    /// for `escapable_caps` lookup.
    ///
    /// Accepts: `T.method(...)` (bare type identifier) — returns `T`;
    /// `v.method(...)` on a tracked `let`-bound instance or function
    /// parameter — returns the type name from `let_types`; any other
    /// receiver shape — falls back to the typechecker's `expr_types`
    /// when available. Returns `None` when no type name can be recovered.
    fn resolve_receiver_type_name(&self, object: &Expr) -> Option<String> {
        if let ExprKind::Identifier(name) = &object.kind {
            if let Some(ty) = self.lookup_let_type(name) {
                return Some(ty.to_string());
            }
            // Fall back to treating the identifier itself as the type
            // name — covers `T.method(...)` static-style calls when the
            // parser emits a MethodCall with an Identifier("T") receiver.
            return Some(name.clone());
        }
        self.receiver_type_name_from_expr(object)
    }

    /// Look up `expr.span` in the typecheck result's `expr_types` and,
    /// if the type is `Named { name, .. }` (possibly wrapped in `Ref` or
    /// `MutRef`), return that name. Returns `None` when the typecheck
    /// result is absent or the type doesn't have a recoverable name.
    ///
    /// Note: `MethodCall` nodes and their receiver sub-expressions
    /// currently share a span (the parser propagates `lhs.span.clone()`
    /// through postfix chains), so looking up a receiver identifier's
    /// span returns the MethodCall's result type, not the identifier's
    /// type. The local `let_types` scope avoids this collision for the
    /// common identifier-receiver case; this helper remains useful for
    /// non-identifier receivers whose spans do not collide.
    fn receiver_type_name_from_expr(&self, expr: &Expr) -> Option<String> {
        let types = self.types?;
        let ty = types.expr_types.get(&SpanKey::from_span(&expr.span))?;
        Self::type_name(ty)
    }

    fn type_name(ty: &Type) -> Option<String> {
        match ty {
            Type::Named { name, .. } => Some(name.clone()),
            Type::Ref(inner) | Type::MutRef(inner) => Self::type_name(inner),
            _ => None,
        }
    }

    /// Plain-identifier assignment variant of `check_escape_expr`.
    /// Triggered only when the target name is known to predate the
    /// innermost rooted frame (see `is_outer_binding`). Same RHS
    /// resolution rules as the field-assignment and return paths.
    fn check_outer_identifier_escape(
        &mut self,
        value: &Expr,
        target_name: &str,
        escape_span: &Span,
    ) {
        if self.rooted.is_empty() {
            return;
        }
        let used_resources = match self.value_escapable_refs(value) {
            Some(refs) if !refs.is_empty() => refs,
            _ => return,
        };
        for (resource, res_span) in self.all_rooted() {
            if used_resources.iter().any(|r| r == &resource) {
                self.errors.push(EscapeError {
                    resource,
                    provider_span: res_span,
                    closure_span: value.span.clone(),
                    escape_span: escape_span.clone(),
                    kind: EscapeKind::OuterIdentifierAssignment {
                        target_name: target_name.to_string(),
                    },
                });
            }
        }
    }

    /// Shared RHS resolution for the escape-at-assignment paths. Returns
    /// the rooted resources captured by `value`, resolved through
    /// closure literals, tracked let-bound identifiers, and
    /// escapable-caps calls / type-method calls. `None` means the RHS
    /// is not a closure-producing expression we recognize.
    fn value_escapable_refs(&self, value: &Expr) -> Option<Vec<String>> {
        match &value.kind {
            ExprKind::Closure { body, .. } => {
                let mut out = Vec::new();
                collect_resource_refs(body, &mut out);
                Some(out)
            }
            ExprKind::Identifier(name) => self.lookup_closure_binding(name).cloned(),
            ExprKind::Call { callee, .. } => self.call_escapable_caps(callee),
            ExprKind::MethodCall { object, method, .. } => {
                let ty_name = self.resolve_receiver_type_name(object)?;
                let key = format!("{}.{}", ty_name, method);
                self.escapable_caps.get(&key).cloned()
            }
            _ => None,
        }
    }

    /// Field-assignment variant of `check_escape_expr`. Same resolution
    /// rules for the RHS (closure literal / let-bound identifier / call
    /// producing an escapable closure) but emits `FieldAssignment` kind
    /// with a target description for the diagnostic.
    fn check_field_assign_escape(&mut self, value: &Expr, target_desc: &str, escape_span: &Span) {
        if self.rooted.is_empty() {
            return;
        }
        let used_resources: Vec<String> = match &value.kind {
            ExprKind::Closure { body, .. } => {
                let mut out = Vec::new();
                collect_resource_refs(body, &mut out);
                out
            }
            ExprKind::Identifier(name) => match self.lookup_closure_binding(name) {
                Some(refs) => refs.clone(),
                None => return,
            },
            ExprKind::Call { callee, .. } => match self.call_escapable_caps(callee) {
                Some(refs) if !refs.is_empty() => refs,
                _ => return,
            },
            ExprKind::MethodCall { object, method, .. } => {
                let Some(ty_name) = self.resolve_receiver_type_name(object) else {
                    return;
                };
                let key = format!("{}.{}", ty_name, method);
                match self.escapable_caps.get(&key) {
                    Some(refs) if !refs.is_empty() => refs.clone(),
                    _ => return,
                }
            }
            _ => return,
        };
        for (resource, res_span) in self.all_rooted() {
            if used_resources.iter().any(|r| r == &resource) {
                self.errors.push(EscapeError {
                    resource,
                    provider_span: res_span,
                    closure_span: value.span.clone(),
                    escape_span: escape_span.clone(),
                    kind: EscapeKind::FieldAssignment {
                        target_desc: target_desc.to_string(),
                    },
                });
            }
        }
    }

    /// Flatten the rooted-scope stack into (resource_name, provider_span)
    /// pairs, innermost first. Used when scanning a closure for captures.
    fn all_rooted(&self) -> Vec<(String, Span)> {
        let mut out = Vec::new();
        for scope in self.rooted.iter().rev() {
            for (name, span) in &scope.resources {
                let anchor = if scope.resources.len() == 1 {
                    scope.block_span.clone()
                } else {
                    span.clone()
                };
                out.push((name.clone(), anchor));
            }
        }
        out
    }
}

/// Recognize the `with_provider[R](provider, closure)` call shape at AST
/// level. Mirror of `Interpreter::match_with_provider`.
fn match_with_provider<'e>(
    callee: &'e Expr,
    args: &'e [CallArg],
) -> Option<(String, &'e Expr, &'e Expr)> {
    let ExprKind::Index { object, index } = &callee.kind else {
        return None;
    };
    let is_with_provider = match &object.kind {
        ExprKind::Identifier(n) => n == "with_provider",
        ExprKind::Path(segs) => segs.as_slice() == ["with_provider"],
        _ => false,
    };
    if !is_with_provider {
        return None;
    }
    let resource = match &index.kind {
        ExprKind::Identifier(n) => n.clone(),
        ExprKind::Path(segs) => segs.last().cloned()?,
        _ => return None,
    };
    if args.len() != 2 {
        return None;
    }
    Some((resource, &args[0].value, &args[1].value))
}

/// Walk an expression, collecting the first segment of every `R.method(...)`
/// call that looks like a resource dispatch. The shape at AST level is
/// `Call(Path([R, method]), args)` after `parse_primary` roots capital-
/// leading idents as paths, matching the dispatch path the interpreter uses.
/// Bare `R.foo()` (MethodCall with Identifier receiver) is also recognized.
fn collect_resource_refs(expr: &Expr, out: &mut Vec<String>) {
    match &expr.kind {
        ExprKind::Call { callee, args } => {
            if let ExprKind::Path(segs) = &callee.kind {
                if segs.len() == 2 {
                    push_unique(out, segs[0].clone());
                }
            }
            collect_resource_refs(callee, out);
            for a in args {
                collect_resource_refs(&a.value, out);
            }
        }
        ExprKind::MethodCall { object, args, .. } => {
            if let ExprKind::Identifier(name) = &object.kind {
                push_unique(out, name.clone());
            }
            collect_resource_refs(object, out);
            for a in args {
                collect_resource_refs(&a.value, out);
            }
        }
        ExprKind::Binary { left, right, .. } | ExprKind::Pipe { left, right } => {
            collect_resource_refs(left, out);
            collect_resource_refs(right, out);
        }
        ExprKind::NilCoalesce { left, right } => {
            collect_resource_refs(left, out);
            collect_resource_refs(right, out);
        }
        ExprKind::Unary { operand, .. } | ExprKind::Question(operand) => {
            collect_resource_refs(operand, out);
        }
        ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
            collect_resource_refs(object, out);
        }
        ExprKind::Index { object, index } => {
            collect_resource_refs(object, out);
            collect_resource_refs(index, out);
        }
        ExprKind::OptionalChain { object, args, .. } => {
            collect_resource_refs(object, out);
            if let Some(args) = args {
                for a in args {
                    collect_resource_refs(&a.value, out);
                }
            }
        }
        ExprKind::Block(b)
        | ExprKind::Unsafe(b)
        | ExprKind::Seq(b)
        | ExprKind::Par(b)
        | ExprKind::Lock { body: b, .. } => collect_block_resource_refs(b, out),
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            collect_resource_refs(condition, out);
            collect_block_resource_refs(then_block, out);
            if let Some(eb) = else_branch {
                collect_resource_refs(eb, out);
            }
        }
        ExprKind::IfLet {
            value,
            then_block,
            else_branch,
            ..
        } => {
            collect_resource_refs(value, out);
            collect_block_resource_refs(then_block, out);
            if let Some(eb) = else_branch {
                collect_resource_refs(eb, out);
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            collect_resource_refs(scrutinee, out);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    collect_resource_refs(g, out);
                }
                collect_resource_refs(&arm.body, out);
            }
        }
        ExprKind::While {
            condition, body, ..
        } => {
            collect_resource_refs(condition, out);
            collect_block_resource_refs(body, out);
        }
        ExprKind::WhileLet { value, body, .. } => {
            collect_resource_refs(value, out);
            collect_block_resource_refs(body, out);
        }
        ExprKind::For { iterable, body, .. } => {
            collect_resource_refs(iterable, out);
            collect_block_resource_refs(body, out);
        }
        ExprKind::Loop { body, .. } => collect_block_resource_refs(body, out),
        ExprKind::Closure { body, .. } => collect_resource_refs(body, out),
        ExprKind::Return(Some(inner))
        | ExprKind::Break {
            value: Some(inner), ..
        } => collect_resource_refs(inner, out),
        ExprKind::Tuple(exprs) | ExprKind::ArrayLiteral(exprs) => {
            for e in exprs {
                collect_resource_refs(e, out);
            }
        }
        ExprKind::RepeatLiteral { value, count, .. } => {
            collect_resource_refs(value, out);
            collect_resource_refs(count, out);
        }
        ExprKind::MapLiteral(entries) => {
            for (k, v) in entries {
                collect_resource_refs(k, out);
                collect_resource_refs(v, out);
            }
        }
        ExprKind::StructLiteral { fields, spread, .. } => {
            for f in fields {
                collect_resource_refs(&f.value, out);
            }
            if let Some(s) = spread {
                collect_resource_refs(s, out);
            }
        }
        ExprKind::Cast { expr: inner, .. } => collect_resource_refs(inner, out),
        ExprKind::Range { start, end, .. } => {
            if let Some(s) = start {
                collect_resource_refs(s, out);
            }
            if let Some(e) = end {
                collect_resource_refs(e, out);
            }
        }
        ExprKind::Providers { bindings, body } => {
            for b in bindings {
                collect_resource_refs(&b.value, out);
            }
            collect_block_resource_refs(body, out);
        }
        _ => {}
    }
}

fn collect_block_resource_refs(block: &Block, out: &mut Vec<String>) {
    for stmt in &block.stmts {
        match &stmt.kind {
            StmtKind::Let { value, .. } => collect_resource_refs(value, out),
            StmtKind::LetUninit { .. } => {}
            StmtKind::LetElse {
                value, else_block, ..
            } => {
                collect_resource_refs(value, out);
                collect_block_resource_refs(else_block, out);
            }
            StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
                collect_resource_refs(target, out);
                collect_resource_refs(value, out);
            }
            StmtKind::Expr(e) => collect_resource_refs(e, out),
            StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                collect_block_resource_refs(body, out);
            }
        }
    }
    if let Some(e) = &block.final_expr {
        collect_resource_refs(e, out);
    }
}

fn push_unique(v: &mut Vec<String>, s: String) {
    if !v.contains(&s) {
        v.push(s);
    }
}

/// Does this assignment-target expression reach a field / index slot that
/// conservatively outlives the current provider scope? Field access,
/// tuple-index, and array/map index are all treated as such. Plain
/// identifiers are excluded — we can't tell here whether the binding is
/// local to the rooted scope or from outside.
fn is_field_like_target(expr: &Expr) -> bool {
    matches!(
        expr.kind,
        ExprKind::FieldAccess { .. } | ExprKind::TupleIndex { .. } | ExprKind::Index { .. }
    )
}

/// Best-effort stringification of a field-like assignment target for the
/// diagnostic message. Handles the common shapes (dotted field chains,
/// tuple indices, and `[expr]` index) and falls back to `"<target>"` for
/// anything unrecognized. Not a formatter — the rendering is lossy by
/// design, since the only consumer is a human-readable error string.
fn render_assign_target(expr: &Expr) -> String {
    match &expr.kind {
        ExprKind::Identifier(name) => name.clone(),
        ExprKind::FieldAccess { object, field } => {
            format!("{}.{}", render_assign_target(object), field)
        }
        ExprKind::TupleIndex { object, index } => {
            format!("{}.{}", render_assign_target(object), index)
        }
        ExprKind::Index { object, .. } => {
            format!("{}[…]", render_assign_target(object))
        }
        ExprKind::Path(segs) => segs.join("."),
        _ => "<target>".to_string(),
    }
}

/// Structural recovery of a type name from a `let`-binding's RHS. Used
/// to feed `let_types` so instance-method calls can dispatch to the
/// right entry in `escapable_caps`.
///
/// Recognized shapes:
/// - Struct literal: `TypeName { … }` — returns `TypeName`.
/// - Static factory call: `TypeName.new(…)` / `TypeName.default()` /
///   `TypeName.anything(…)` where the callee is either a `Path` or a
///   `MethodCall` with an Identifier receiver naming the type.
/// - Path-form: `TypeName { ... }` using a segmented path — returns the
///   last segment.
///
/// Unrecognized shapes (closures, literals, arithmetic, arbitrary calls)
/// return `None` — the instance-method resolver falls through to other
/// mechanisms, and the check stays conservative.
fn extract_type_name_from_let_value(value: &Expr) -> Option<String> {
    match &value.kind {
        ExprKind::StructLiteral { path, .. } => path.last().cloned(),
        ExprKind::Call { callee, .. } => match &callee.kind {
            ExprKind::Path(segs) if segs.len() == 2 => Some(segs[0].clone()),
            ExprKind::MethodCall { object, .. } => {
                if let ExprKind::Identifier(ty) = &object.kind {
                    Some(ty.clone())
                } else {
                    None
                }
            }
            _ => None,
        },
        ExprKind::MethodCall { object, .. } => {
            // Surface form `TypeName.new()` — parser emits a MethodCall
            // whose object is an Identifier for the type name.
            if let ExprKind::Identifier(ty) = &object.kind {
                Some(ty.clone())
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Recover the underlying user-defined type name from a parameter's
/// `TypeExpr`, stripping `ref` / `mut ref` / `weak` wrappers. Returns
/// `None` for tuples, function types, or unresolved forms.
fn type_expr_name(ty: &TypeExpr) -> Option<String> {
    match &ty.kind {
        TypeKind::Path(p) => p.segments.last().cloned(),
        TypeKind::Ref(inner) | TypeKind::MutRef(inner) | TypeKind::Weak(inner) => {
            type_expr_name(inner)
        }
        _ => None,
    }
}
