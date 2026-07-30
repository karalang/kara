//! Iteration-local provenance analysis for auto-par reduction bodies.
//!
//! B-2026-07-30-1. The auto-par soundness gate that guards reduction
//! fan-out against racing a NON-ATOMIC refcount
//! (`ConcurrencyChecker::loop_body_types_cross_task_safe`, the
//! B-2026-07-16-6 gate) is purely TYPE-based: it declines the reduction
//! when any typed expression inside the loop body has a
//! cross-task-unsafe type. Declining is correct for a handle that
//! actually crosses workers — racing rc-inc/rc-dec on a `shared`
//! header is a lost update and therefore a use-after-free.
//!
//! It is imprecise for the opposite case, which is common: a `shared`
//! value ALLOCATED and fully consumed inside ONE iteration never has a
//! handle reach a second worker, so its non-atomic header is only ever
//! touched by the thread that made it. Any kata that builds a linked
//! list / tree per iteration and folds it hits this — measured on
//! kara-katas #23 (merge-k-sorted-lists) and #86 (partition-list),
//! whose par lanes got 1.00x / 1.02x against peers reaching 2.0–2.9x.
//!
//! This module supplies the missing escape analysis. It answers one
//! question: *which expressions in this loop body provably cannot
//! evaluate to a handle on an object that existed before the iteration
//! began, or that outlives it?* The gate then declines only when an
//! unsafe-typed expression is NOT in that set.
//!
//! ## Soundness shape: a fail-CLOSED whitelist, not a fail-open walk
//!
//! The type gate's doc comment records why it is a span sweep rather
//! than an AST walk: a walk has to enumerate every `ExprKind`, and a
//! missed variant silently reopens the soundness hole. That property is
//! preserved here by inverting the polarity. This walk never *clears* an
//! expression the sweep flagged; it only *adds* spans to a whitelist,
//! and it adds a span only on POSITIVE evidence of freshness. An
//! `ExprKind` this module does not know about — including any variant
//! the language grows tomorrow — falls through to `false`, i.e. "not
//! provably iteration-local", i.e. the sweep's decline stands. A missed
//! variant costs a sequential loop, never a miscompile.
//!
//! ## The two flows that have to be closed
//!
//! **INFLOW** — a pre-iteration object entering the iteration. Every
//! such object has to be *named* to be reached: a parameter, `self`, an
//! outer local, or a projection of one. So [`Expr`] freshness is rooted
//! at name provenance: an identifier is iteration-local only when it is
//! bound inside the body AND every value that can flow into it is
//! itself iteration-local. Provenance is a GREATEST fixpoint
//! ([`Provenance::solve`]) so it is order-insensitive and handles
//! loop-carried bindings (`cur = n.next` inside a nested walk loop)
//! without a phase-ordering bug.
//!
//! **OUTFLOW** — a handle surviving the iteration. Writing into a
//! local's field (`node.next = X`) is treated as a write to the local
//! itself, so a non-local `X` demotes `node`. Everything that can carry
//! a value out of the body wholesale — `return`, `break value`, a
//! closure, a nested `par` / `spawn` / `lock` / provider region — sets
//! [`Provenance::escapes`] and the whole analysis declines.
//!
//! Writes to an OUTER name need no separate check: they are the
//! reduction accumulator (nothing else survives
//! `ConcurrencyChecker::classify_loop_body`), and the accumulator is an
//! outer name, so its own mentions are swept. An unsafe-typed
//! accumulator therefore declines on its own reads; a safe-typed one
//! cannot hold an unsafe value, by typing.
//!
//! ## What is NOT claimed
//!
//! This is strictly a precision improvement layered on top of the type
//! gate, which stays as the fallback for everything not proven fresh.
//! Do not weaken the type gate itself — B-2026-07-16-6 documents the
//! use-after-free it prevents.

use crate::ast::*;
use crate::resolver::SpanKey;
use std::collections::{HashMap, HashSet};

/// Provenance facts gathered from one loop body in a single pass.
///
/// `sources[n]` is every expression that can flow into the body-bound
/// name `n` — its `let` initializer, every later assignment, the
/// scrutinee/iterable a pattern destructures, and (conservatively) the
/// right-hand side of any write into a *place rooted at* `n`
/// (`n.next = X` records `X` as a source of `n`, because after it `n`
/// transitively reaches whatever `X` reached).
///
/// A name with an entry is a *candidate* local. A name with an empty
/// source list (`let uninit`) is local: nothing can flow in.
#[derive(Default)]
struct Provenance<'a> {
    sources: HashMap<String, Vec<&'a Expr>>,
    /// Set when the body contains a construct that can carry a value out
    /// of the iteration, or a region this analysis does not model. Any
    /// occurrence declines the whole body.
    escapes: bool,
}

/// Names that resolve to a *unit* enum variant (`None`, `Tree.Leaf`).
///
/// These matter because a unit variant of an enum with a `shared`
/// payload elsewhere carries the enum's own — unsafe — type while
/// holding no handle at all: `None` has type `Option[shared Node]`. Not
/// whitelisting it would decline every `Option[shared T]` linked-list
/// shape at its very first `let head = None`.
///
/// Constructing an enum value is a fresh allocation either way, so a
/// unit-variant mention is unconditionally iteration-local.
fn unit_variant_names(tc: &crate::typechecker::TypeCheckResult) -> HashSet<String> {
    let mut out = HashSet::new();
    for info in tc.enum_info.values() {
        for (name, shape) in &info.variants {
            if matches!(shape, crate::typechecker::types::VariantTypeInfo::Unit) {
                out.insert(name.clone());
            }
        }
    }
    out
}

/// Everything [`is_local`] consults.
///
/// `locals` doubles as the provenance fixpoint's working set:
/// [`Provenance::solve`] seeds it with every candidate and shrinks it in
/// place, so the same `is_local` used for the final whitelist is the one
/// used to converge. There is no second, subtly-different predicate to
/// keep in sync.
struct Ctx<'a> {
    tc: &'a crate::typechecker::TypeCheckResult,
    locals: HashSet<String>,
    units: HashSet<String>,
}

impl Ctx<'_> {
    /// True when `expr`'s recorded type cannot transitively contain a
    /// cross-task-unsafe leaf — so the value carries no non-atomic
    /// refcount to race on, whatever its provenance.
    ///
    /// This is the base case that makes the structural rules usable: a
    /// loop body is full of plain scalars (`k + j`, `acc`, `n.val`) that
    /// are outer-scope reads and would otherwise poison every derived
    /// expression, even though an `i64` cannot possibly alias a `shared`
    /// object. `None` (no recorded type) falls through to the structural
    /// rules, which are conservative.
    fn type_is_safe(&self, expr: &Expr) -> bool {
        let key = SpanKey(expr.span.offset, expr.span.length);
        self.tc
            .expr_types
            .get(&key)
            .is_some_and(|ty| crate::cross_task_safe::is_cross_task_safe(ty, self.tc).is_ok())
    }
}

/// Spans of every expression in `body` that provably cannot evaluate to
/// a handle on an object from outside the current iteration.
///
/// Returns `None` when the body escapes (see [`Provenance::escapes`]) —
/// the caller must then decline on the type gate alone.
pub fn iteration_local_spans(
    body: &Block,
    tc: &crate::typechecker::TypeCheckResult,
) -> Option<HashSet<SpanKey>> {
    let mut prov = Provenance::default();
    prov.scan_block(body);
    if prov.escapes {
        return None;
    }
    let mut cx = Ctx {
        tc,
        locals: HashSet::new(),
        units: unit_variant_names(tc),
    };
    prov.solve(&mut cx);
    let mut walker = LocalWalker {
        cx: &cx,
        spans: HashSet::new(),
    };
    walker.block(body);
    Some(walker.spans)
}

// ── Pass 1: provenance + escape scan ──────────────────────────────────

impl<'a> Provenance<'a> {
    fn note_pattern(&mut self, pattern: &Pattern, source: &'a Expr) {
        let mut names = HashSet::new();
        collect_pattern_bindings(pattern, &mut names);
        for n in names {
            self.sources.entry(n).or_default().push(source);
        }
    }

    /// Record a write of `value` into `target`. An identifier target is
    /// a direct rebind; a projection target (`n.f`, `n[i]`, `*n`) is
    /// recorded against the projection's ROOT name, because after the
    /// write that root transitively reaches whatever `value` reached.
    fn note_write(&mut self, target: &Expr, value: &'a Expr) {
        if let Some(root) = place_root_name(target) {
            // No entry means an OUTER name — only the reduction
            // accumulator and the loop counter survive
            // `classify_loop_body`, and both are handled by the type
            // sweep rather than here (see module docs).
            if let Some(slot) = self.sources.get_mut(&root) {
                slot.push(value);
            }
        }
    }

    fn scan_block(&mut self, block: &'a Block) {
        for stmt in &block.stmts {
            self.scan_stmt(stmt);
        }
        if let Some(e) = &block.final_expr {
            self.scan_expr(e);
        }
    }

    fn scan_stmt(&mut self, stmt: &'a Stmt) {
        match &stmt.kind {
            StmtKind::Let { pattern, value, .. } => {
                self.scan_expr(value);
                self.note_pattern(pattern, value);
            }
            StmtKind::LetElse {
                pattern,
                value,
                else_block,
                ..
            } => {
                self.scan_expr(value);
                self.note_pattern(pattern, value);
                self.scan_block(else_block);
            }
            StmtKind::LetUninit { name, .. } => {
                // Nothing can have flowed in yet; later assignments are
                // recorded by `note_write`. An empty source list is the
                // "local, no inflow" base case the fixpoint starts from.
                self.sources.entry(name.clone()).or_default();
            }
            StmtKind::Assign { target, value } => {
                self.scan_expr(target);
                self.scan_expr(value);
                self.note_write(target, value);
            }
            StmtKind::CompoundAssign { target, value, .. } => {
                self.scan_expr(target);
                self.scan_expr(value);
                self.note_write(target, value);
            }
            StmtKind::Expr(e) => self.scan_expr(e),
            StmtKind::Defer { .. } | StmtKind::ErrDefer { .. } => {
                // A defer runs at scope exit, outside the per-iteration
                // window this analysis reasons about.
                self.escapes = true;
            }
            StmtKind::MultiAssign { .. } => {
                // Removed by the desugar pass before this phase; treat a
                // surprise occurrence as unanalyzable rather than
                // silently ignoring its writes.
                self.escapes = true;
            }
        }
    }

    fn scan_expr(&mut self, expr: &'a Expr) {
        match &expr.kind {
            // Value-carrying exits and regions this analysis does not
            // model. `Closure` captures by reference into a value that
            // outlives the expression; `Par` / `Seq` / `Lock` /
            // `Providers` / `Comptime` are separate concurrency or
            // evaluation contexts with their own capture rules.
            ExprKind::Return(_)
            | ExprKind::Closure { .. }
            | ExprKind::Par(_)
            | ExprKind::Seq(_)
            | ExprKind::Lock { .. }
            | ExprKind::Providers { .. }
            | ExprKind::Comptime(_) => {
                self.escapes = true;
            }
            ExprKind::Break { value, .. } => {
                if value.is_some() {
                    self.escapes = true;
                }
            }

            ExprKind::For {
                pattern,
                iterable,
                body,
                ..
            } => {
                self.scan_expr(iterable);
                self.note_pattern(pattern, iterable);
                self.scan_block(body);
            }
            ExprKind::IfLet {
                pattern,
                value,
                then_block,
                else_branch,
            } => {
                self.scan_expr(value);
                self.note_pattern(pattern, value);
                self.scan_block(then_block);
                if let Some(e) = else_branch {
                    self.scan_expr(e);
                }
            }
            ExprKind::WhileLet {
                pattern,
                value,
                body,
                ..
            } => {
                self.scan_expr(value);
                self.note_pattern(pattern, value);
                self.scan_block(body);
            }
            ExprKind::Match { scrutinee, arms } => {
                self.scan_expr(scrutinee);
                for arm in arms {
                    self.note_pattern(&arm.pattern, scrutinee);
                    if let Some(g) = &arm.guard {
                        self.scan_expr(g);
                    }
                    self.scan_expr(&arm.body);
                }
            }

            _ => visit_children(expr, self),
        }
    }

    /// Resolve `cx.locals` to the greatest fixpoint: seed it with every
    /// candidate, then drop any name whose sources are not all
    /// iteration-local, until stable. Each round removes at least one
    /// name from a finite set, so it terminates.
    ///
    /// Greatest (shrinking) rather than least (growing) is what makes a
    /// loop-carried binding work: `cur = n.next` inside a nested walk
    /// mentions names that are only local if `cur` itself is. A least
    /// fixpoint would never admit the cycle; a greatest one keeps it
    /// unless some source genuinely reaches outside.
    fn solve(&self, cx: &mut Ctx<'_>) {
        cx.locals = self.sources.keys().cloned().collect();
        while let Some(demoted) = self.sources.iter().find_map(|(name, srcs)| {
            (cx.locals.contains(name) && !srcs.iter().all(|s| is_local(s, cx)))
                .then(|| name.clone())
        }) {
            cx.locals.remove(&demoted);
        }
    }
}

impl<'a> Visit<'a> for Provenance<'a> {
    fn on_expr(&mut self, expr: &'a Expr) {
        self.scan_expr(expr);
    }
    fn on_block(&mut self, block: &'a Block) {
        self.scan_block(block);
    }
}

// ── Pass 2: whitelist collection ──────────────────────────────────────

struct LocalWalker<'a> {
    cx: &'a Ctx<'a>,
    spans: HashSet<SpanKey>,
}

impl LocalWalker<'_> {
    fn block(&mut self, block: &Block) {
        for stmt in &block.stmts {
            match &stmt.kind {
                StmtKind::Let { value, .. } | StmtKind::LetElse { value, .. } => self.expr(value),
                StmtKind::Assign { target, value } => {
                    self.expr(target);
                    self.expr(value);
                }
                StmtKind::CompoundAssign { target, value, .. } => {
                    self.expr(target);
                    self.expr(value);
                }
                StmtKind::Expr(e) => self.expr(e),
                _ => {}
            }
            if let StmtKind::LetElse { else_block, .. } = &stmt.kind {
                self.block(else_block);
            }
        }
        if let Some(e) = &block.final_expr {
            self.expr(e);
        }
    }

    /// Record `expr`'s span when it is iteration-local, then recurse so
    /// every subexpression is judged on its own merits. Recursion is
    /// unconditional: a non-local parent can still contain local
    /// children (`outer_vec[fresh_index]`), and a local parent's
    /// children were already proven local by [`is_local`].
    fn expr(&mut self, expr: &Expr) {
        if is_local(expr, self.cx) {
            self.spans
                .insert(SpanKey(expr.span.offset, expr.span.length));
        }
        visit_children(expr, self);
    }
}

impl<'e> Visit<'e> for LocalWalker<'_> {
    fn on_expr(&mut self, expr: &'e Expr) {
        self.expr(expr);
    }
    fn on_block(&mut self, block: &'e Block) {
        self.block(block);
    }
}

// ── The freshness predicate ───────────────────────────────────────────

/// True when `expr` provably cannot evaluate to a handle on an object
/// that existed before the current iteration.
///
/// Every arm is positive evidence. The catch-all is `false`, so an
/// unlisted `ExprKind` — today's or tomorrow's — is treated as possibly
/// aliasing, and the type gate's decline stands.
fn is_local(expr: &Expr, cx: &Ctx<'_>) -> bool {
    // Type-level short-circuit first: a value whose type holds no
    // cross-task-unsafe leaf carries no refcount to race on, so its
    // provenance is irrelevant. Without this, ordinary scalar reads of
    // outer bindings (`k + j`, the loop counter) would count as
    // non-local and poison every expression built from them.
    if cx.type_is_safe(expr) {
        return true;
    }
    match &expr.kind {
        // Literals carry no handle.
        ExprKind::Integer(..)
        | ExprKind::Float(..)
        | ExprKind::CharLit(_)
        | ExprKind::ByteLit(_)
        | ExprKind::StringLit(_)
        | ExprKind::MultiStringLit(_)
        | ExprKind::CStringLit { .. }
        | ExprKind::Bool(_) => true,
        ExprKind::InterpolatedStringLit(parts) => parts.iter().all(|p| match p {
            ParsedInterpolationPart::Text(_) => true,
            ParsedInterpolationPart::Expr(e, _) => is_local(e, cx),
        }),

        // A name is local when it is body-bound with wholly local
        // provenance, or when it denotes a unit enum variant (a fresh
        // payload-free value that merely carries the enum's type).
        ExprKind::Identifier(name) => cx.locals.contains(name) || cx.units.contains(name),
        ExprKind::Path { segments, .. } => {
            segments.last().is_some_and(|tail| cx.units.contains(tail))
        }

        // Fresh allocations. Field/element expressions are judged
        // separately by the walk, so a literal whose field reads an
        // outer handle still declines — on that field, not on the
        // literal.
        ExprKind::StructLiteral { fields, spread, .. } => {
            fields.iter().all(|f| is_local(&f.value, cx))
                && spread.as_ref().is_none_or(|s| is_local(s, cx))
        }
        ExprKind::Tuple(items) | ExprKind::ArrayLiteral(items) => {
            items.iter().all(|e| is_local(e, cx))
        }
        ExprKind::PrefixCollectionLiteral { items, .. } => items.iter().all(|e| is_local(e, cx)),
        ExprKind::RepeatLiteral { value, count, .. } => is_local(value, cx) && is_local(count, cx),
        ExprKind::MapLiteral(entries) => entries
            .iter()
            .all(|(k, v)| is_local(k, cx) && is_local(v, cx)),

        // A call's result can only alias what the callee could reach.
        // Kāra has no mutable module-level state (`ConstDecl` is the
        // only item-level binding and it is immutable), so a callee
        // reaches exactly its arguments plus whatever it allocates
        // itself. All-local arguments therefore imply an all-local
        // result. The callee expression must be a plain name/path — a
        // function VALUE read out of an outer place is not evidence
        // about what it captured.
        ExprKind::Call { callee, args } => {
            matches!(
                callee.kind,
                ExprKind::Identifier(_) | ExprKind::Path { .. } | ExprKind::SelfType
            ) && args.iter().all(|a| is_local(&a.value, cx))
        }
        ExprKind::MethodCall { object, args, .. } => {
            is_local(object, cx) && args.iter().all(|a| is_local(&a.value, cx))
        }

        // Projections of a local object. A write of a non-local value
        // into any such place demotes the root (`Provenance::note_write`),
        // so reaching through a still-local root stays sound.
        ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
            is_local(object, cx)
        }
        ExprKind::Index { object, index } => is_local(object, cx) && is_local(index, cx),

        // Operators and coercions over local operands. `Deref` is
        // excluded on purpose: a raw pointer carries no provenance, so
        // `*p` is no evidence about the pointee.
        ExprKind::Binary { left, right, .. } => is_local(left, cx) && is_local(right, cx),
        ExprKind::Unary { op, operand } => !matches!(op, UnaryOp::Deref) && is_local(operand, cx),
        ExprKind::Question(inner) | ExprKind::Cast { expr: inner, .. } => is_local(inner, cx),
        ExprKind::OptionalChain { object, .. } => is_local(object, cx),
        ExprKind::NilCoalesce { left, right } => is_local(left, cx) && is_local(right, cx),
        ExprKind::Range { start, end, .. } => {
            start.as_ref().is_none_or(|e| is_local(e, cx))
                && end.as_ref().is_none_or(|e| is_local(e, cx))
        }

        // Block-valued forms: local iff every value-producing position is.
        ExprKind::Block(b) | ExprKind::Unsafe(b) => block_is_local(b, cx),
        ExprKind::If {
            then_block,
            else_branch,
            ..
        } => block_is_local(then_block, cx) && else_branch.as_ref().is_none_or(|e| is_local(e, cx)),
        ExprKind::IfLet {
            then_block,
            else_branch,
            ..
        } => block_is_local(then_block, cx) && else_branch.as_ref().is_none_or(|e| is_local(e, cx)),
        ExprKind::Match { arms, .. } => arms.iter().all(|a| is_local(&a.body, cx)),

        // Statement-like loops evaluate to unit.
        ExprKind::While { .. } | ExprKind::WhileLet { .. } | ExprKind::For { .. } => true,

        // Everything else — `Loop` (its value comes from `break`),
        // `Closure`, `Par`, `Seq`, `Lock`, `Providers`, `Comptime`,
        // `Try`, `LabeledBlock`, `Pipe`, `Return`, `Break`, `Continue`,
        // `SelfValue`, `SelfType`, `OffsetOf`, `PipePlaceholder`,
        // `Error`, and any variant added later — is not proven fresh.
        _ => false,
    }
}

fn block_is_local(block: &Block, cx: &Ctx<'_>) -> bool {
    block.final_expr.as_ref().is_none_or(|e| is_local(e, cx))
}

/// Root name of a place expression (`n`, `n.f`, `n[i]`, `n.f[i].g`).
/// `None` when the place is not rooted at a plain name.
fn place_root_name(expr: &Expr) -> Option<String> {
    match &expr.kind {
        ExprKind::Identifier(name) => Some(name.clone()),
        ExprKind::FieldAccess { object, .. }
        | ExprKind::TupleIndex { object, .. }
        | ExprKind::Index { object, .. } => place_root_name(object),
        ExprKind::Unary {
            op: UnaryOp::Deref,
            operand,
        } => place_root_name(operand),
        _ => None,
    }
}

/// Names a pattern binds. Mirrors
/// `ConcurrencyChecker::collect_pattern_bindings`; kept as a free
/// function so this module needs no checker handle.
fn collect_pattern_bindings(pattern: &Pattern, out: &mut HashSet<String>) {
    match &pattern.kind {
        PatternKind::Binding(name) => {
            out.insert(name.clone());
        }
        PatternKind::AtBinding { name, pattern, .. } => {
            out.insert(name.clone());
            collect_pattern_bindings(pattern, out);
        }
        PatternKind::Struct { fields, .. } => {
            for f in fields {
                match &f.pattern {
                    Some(p) => collect_pattern_bindings(p, out),
                    None => {
                        out.insert(f.name.clone());
                    }
                }
            }
        }
        PatternKind::TupleVariant { patterns, .. }
        | PatternKind::Tuple(patterns)
        | PatternKind::Or(patterns) => {
            for p in patterns {
                collect_pattern_bindings(p, out);
            }
        }
        PatternKind::Slice {
            prefix,
            rest,
            suffix,
        } => {
            for p in prefix.iter().chain(suffix.iter()) {
                collect_pattern_bindings(p, out);
            }
            if let Some(RestPattern::Bound(name)) = rest {
                out.insert(name.clone());
            }
        }
        PatternKind::Wildcard | PatternKind::Literal(_) | PatternKind::RangePattern { .. } => {}
    }
}

/// Sink for [`visit_children`]. A trait rather than a pair of closures
/// because both passes are `&mut self` methods on one struct, and two
/// closures capturing the same receiver can't coexist.
trait Visit<'e> {
    fn on_expr(&mut self, expr: &'e Expr);
    fn on_block(&mut self, block: &'e Block);
}

/// Structural recursion shared by both passes: invoke `v.on_expr` for
/// every immediate subexpression of `expr` and `v.on_block` for every
/// immediate subblock.
///
/// Exhaustive over `ExprKind` on purpose — the `match` has no catch-all,
/// so a new variant is a compile error here rather than a silently
/// unvisited subtree. (Missing a subtree would only ever *shrink* the
/// whitelist, so it is not a soundness hole, but it is a precision bug
/// worth a compiler error.)
fn visit_children<'e>(expr: &'e Expr, v: &mut impl Visit<'e>) {
    match &expr.kind {
        ExprKind::Integer(..)
        | ExprKind::Float(..)
        | ExprKind::CharLit(_)
        | ExprKind::ByteLit(_)
        | ExprKind::StringLit(_)
        | ExprKind::MultiStringLit(_)
        | ExprKind::CStringLit { .. }
        | ExprKind::Bool(_)
        | ExprKind::Identifier(_)
        | ExprKind::Path { .. }
        | ExprKind::SelfValue
        | ExprKind::SelfType
        | ExprKind::PipePlaceholder
        | ExprKind::Continue { .. }
        | ExprKind::OffsetOf { .. }
        | ExprKind::Error => {}

        ExprKind::InterpolatedStringLit(parts) => {
            for p in parts {
                if let ParsedInterpolationPart::Expr(e, _) = p {
                    v.on_expr(e);
                }
            }
        }
        ExprKind::Binary { left, right, .. } | ExprKind::NilCoalesce { left, right } => {
            v.on_expr(left);
            v.on_expr(right);
        }
        ExprKind::Pipe { left, right } => {
            v.on_expr(left);
            v.on_expr(right);
        }
        ExprKind::Unary { operand, .. } => v.on_expr(operand),
        ExprKind::Question(inner) => v.on_expr(inner),
        ExprKind::Cast { expr: inner, .. } => v.on_expr(inner),
        ExprKind::OptionalChain { object, .. } => v.on_expr(object),
        ExprKind::Call { callee, args } => {
            v.on_expr(callee);
            for a in args {
                v.on_expr(&a.value);
            }
        }
        ExprKind::MethodCall { object, args, .. } => {
            v.on_expr(object);
            for a in args {
                v.on_expr(&a.value);
            }
        }
        ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
            v.on_expr(object)
        }
        ExprKind::Index { object, index } => {
            v.on_expr(object);
            v.on_expr(index);
        }
        ExprKind::Block(b)
        | ExprKind::Comptime(b)
        | ExprKind::Unsafe(b)
        | ExprKind::Try(b)
        | ExprKind::Seq(b)
        | ExprKind::Par(b) => v.on_block(b),
        ExprKind::LabeledBlock { body, .. } => v.on_block(body),
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            v.on_expr(condition);
            v.on_block(then_block);
            if let Some(e) = else_branch {
                v.on_expr(e);
            }
        }
        ExprKind::IfLet {
            value,
            then_block,
            else_branch,
            ..
        } => {
            v.on_expr(value);
            v.on_block(then_block);
            if let Some(e) = else_branch {
                v.on_expr(e);
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            v.on_expr(scrutinee);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    v.on_expr(g);
                }
                v.on_expr(&arm.body);
            }
        }
        ExprKind::While {
            condition, body, ..
        } => {
            v.on_expr(condition);
            v.on_block(body);
        }
        ExprKind::WhileLet { value, body, .. } => {
            v.on_expr(value);
            v.on_block(body);
        }
        ExprKind::For { iterable, body, .. } => {
            v.on_expr(iterable);
            v.on_block(body);
        }
        ExprKind::Loop { body, .. } => v.on_block(body),
        ExprKind::Closure { body, .. } => v.on_expr(body),
        ExprKind::Return(value) => {
            if let Some(e) = value {
                v.on_expr(e);
            }
        }
        ExprKind::Break { value, .. } => {
            if let Some(e) = value {
                v.on_expr(e);
            }
        }
        ExprKind::Tuple(items) | ExprKind::ArrayLiteral(items) => {
            for e in items {
                v.on_expr(e);
            }
        }
        ExprKind::PrefixCollectionLiteral { items, .. } => {
            for e in items {
                v.on_expr(e);
            }
        }
        ExprKind::RepeatLiteral { value, count, .. } => {
            v.on_expr(value);
            v.on_expr(count);
        }
        ExprKind::MapLiteral(entries) => {
            for (key, value) in entries {
                v.on_expr(key);
                v.on_expr(value);
            }
        }
        ExprKind::StructLiteral { fields, spread, .. } => {
            for f in fields {
                v.on_expr(&f.value);
            }
            if let Some(s) = spread {
                v.on_expr(s);
            }
        }
        ExprKind::Range { start, end, .. } => {
            if let Some(e) = start {
                v.on_expr(e);
            }
            if let Some(e) = end {
                v.on_expr(e);
            }
        }
        ExprKind::Lock { body, .. } => v.on_block(body),
        ExprKind::Providers { bindings, body } => {
            for b in bindings {
                v.on_expr(&b.value);
            }
            v.on_block(body);
        }
    }
}
