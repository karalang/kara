//! RC elision — phase A: trivial intra-fn single-owner analysis.
//!
//! Design record: `docs/implementation_checklist/phase-7-codegen.md`
//! § "RC elision for provably-single-owner `shared struct` values"
//! (design locked 2026-06-05). This module implements **phase A** only:
//! a `shared struct` binding is *elidable* when the analysis proves its
//! refcount can never be observed above 1 by any live reference — the
//! value is then a plain owned heap allocation and codegen replaces its
//! scope-exit `RcDec` (load + dec + store + cmp + branch + drop-fn)
//! with an unconditional `free`. The rc header is KEPT and initialized
//! (layout uniformity across elided/non-elided values — see design
//! decision 2); only count *operations* are elided.
//!
//! ## Phase-A candidate predicate (ALL must hold at birth)
//!
//! - born from a `shared struct` literal bound by a plain
//!   `let <name> = S { ... };`
//! - `S` is `shared struct` (not `par struct` — those cross tasks by
//!   design) with **all-primitive fields** (no heap-owning or shared
//!   fields, so the elided free needs no field walk and nothing the
//!   recursive drop would have dec'd is skipped)
//! - `S` has no user `impl Drop` (UserDrop interacts with the drop
//!   path — deferred)
//! - the name is bound exactly once in the function (no rebinds, no
//!   param shadowing — the analysis is name-keyed, mirroring codegen's
//!   name-keyed cleanup tables)
//!
//! ## Use rules (default-deny)
//!
//! The body walk **allows** exactly four use shapes:
//! 1. `x.field` reads (`FieldAccess` with the candidate as object) —
//!    primitive copies out, no count effect;
//! 2. `x.field = v` / `x.field op= v` writes (primitive field stores);
//! 3. `x` as a call argument whose INFERRED parameter mode is
//!    `ref` / `mut ref`. `param_modes` carries body-usage inference
//!    (the would-be-mode machinery), so this is deliberately stronger
//!    than the declared mode: a declared-owned param whose body only
//!    reads infers `Ref` and is safe — the callee's receive-inc /
//!    scope-exit dec self-balance, and an inferred non-`Own` mode
//!    proves the body never consumes, stores, or returns the param
//!    (any of those infer `Own`). Unknown callees (builtins, synth
//!    Display, unresolved keys) block conservatively;
//! 4. `x.method(...)` where the method's receiver is `ref self` /
//!    `mut ref self` (same borrow argument).
//!
//! Every other appearance of the bare identifier blocks elision:
//! aliasing lets, owned args (callee could retain), owned-`self`
//! receivers, returns/tails, stores into any aggregate, match/if-let
//! scrutinees — all collapse into the bare-identifier default. Closure
//! bodies and `par {}` regions block *any* candidate mention (capture /
//! cross-task escape — the same boundary set the Rc→Arc walker guards;
//! `spawn(...)`/`tx.send(...)` move values via closures or owned args,
//! so those boundaries are covered by rules 3/„closure" without naming
//! them). Any AST construct this walker does not explicitly enumerate
//! poisons every candidate in the function (`unhandled construct`) —
//! soundness never depends on the walker being complete.
//!
//! Block reasons are recorded as data (`ElisionBlocked`) per design
//! decision 5 — no CLI surface yet; the records exist for phase-B/C
//! corpus tuning and a future `karac explain` integration.

use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::token::Span;
use crate::typechecker::Type;

use super::{OwnershipChecker, OwnershipMode};

/// Why a phase-A candidate was rejected. Recorded once per binding
/// (first reason wins) in `OwnershipCheckResult::elision_blocked`.
#[derive(Debug, Clone)]
pub struct ElisionBlocked {
    pub binding: String,
    pub reason: String,
    pub span: Span,
}

/// Per-function scan state. Candidates start optimistic and get
/// removed (with a recorded reason) as disqualifying uses surface.
struct ElisionScan {
    /// candidate name → struct type name (for method self-mode lookups).
    candidates: HashMap<String, String>,
    blocked: Vec<ElisionBlocked>,
    /// Names seen bound by ANY binding form (lets of all kinds, params,
    /// loop/match pattern bindings). A second sighting of a candidate
    /// name disqualifies it — the analysis is name-keyed.
    bound_names: HashSet<String>,
}

impl ElisionScan {
    fn block(&mut self, name: &str, reason: &str, span: &Span) {
        if self.candidates.remove(name).is_some() {
            self.blocked.push(ElisionBlocked {
                binding: name.to_string(),
                reason: reason.to_string(),
                span: span.clone(),
            });
        }
    }

    /// Poison every remaining candidate — used for constructs the
    /// walker doesn't enumerate. Conservative-by-construction: an
    /// unknown construct could hide an escape.
    fn poison_all(&mut self, reason: &str, span: &Span) {
        let names: Vec<String> = self.candidates.keys().cloned().collect();
        for n in names {
            self.block(&n, reason, span);
        }
    }
}

/// Walk context flags. Both are blanket blocks for any candidate
/// mention in the subtree.
#[derive(Clone, Copy, Default)]
struct Ctx {
    in_closure: bool,
    in_par: bool,
}

impl<'a> OwnershipChecker<'a> {
    /// Phase-A driver — runs after `check_items` (so `param_modes` is
    /// populated for the ref-arg rule) over every function and impl
    /// method, mirroring `check_items`' iteration shape.
    pub(crate) fn compute_elision(&mut self) {
        let mut elided: HashMap<String, HashSet<String>> = HashMap::new();
        let mut blocked: HashMap<String, Vec<ElisionBlocked>> = HashMap::new();
        for item in &self.program.items {
            match item {
                Item::Function(f) => {
                    let (e, b) = self.fn_elision(f);
                    if !e.is_empty() {
                        elided.insert(f.name.clone(), e);
                    }
                    if !b.is_empty() {
                        blocked.insert(f.name.clone(), b);
                    }
                }
                Item::ImplBlock(imp) => {
                    let type_name = match &imp.target_type.kind {
                        TypeKind::Path(p) => p.segments.last().cloned().unwrap_or_default(),
                        _ => continue,
                    };
                    for item in &imp.items {
                        if let ImplItem::Method(method) = item {
                            let fn_key = format!("{}.{}", type_name, method.name);
                            let (e, b) = self.fn_elision(method);
                            if !e.is_empty() {
                                elided.insert(fn_key.clone(), e);
                            }
                            if !b.is_empty() {
                                blocked.insert(fn_key, b);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        self.elided_bindings = elided;
        self.elision_blocked = blocked;
    }

    fn fn_elision(&self, f: &Function) -> (HashSet<String>, Vec<ElisionBlocked>) {
        let mut scan = ElisionScan {
            candidates: HashMap::new(),
            blocked: Vec::new(),
            bound_names: HashSet::new(),
        };
        // Params shadow-disqualify by name.
        for p in &f.params {
            for n in p.pattern.binding_names() {
                scan.bound_names.insert(n);
            }
        }
        // Pass 1: collect candidates + every bound name.
        self.collect_candidates_in_block(&f.body, &mut scan);
        // Pass 2: use walk (default-deny).
        self.scan_block(&f.body, Ctx::default(), &mut scan);
        (scan.candidates.into_keys().collect(), scan.blocked)
    }

    /// True when `name` is a `shared struct` (not `par`), all of whose
    /// fields are primitive (no heap, no shared, no aggregates), with
    /// no user `impl Drop`.
    fn elision_eligible_struct(&self, name: &str) -> bool {
        let Some(info) = self.typecheck_result.struct_info.get(name) else {
            return false;
        };
        if !info.is_shared || info.is_par {
            return false;
        }
        if self.typecheck_result.drop_method_keys.contains_key(name) {
            return false;
        }
        info.fields.iter().all(|(_, ty, _)| {
            matches!(
                ty,
                Type::Int(_) | Type::UInt(_) | Type::Float(_) | Type::Bool | Type::Char
            )
        })
    }

    // ── Pass 1: candidate collection ────────────────────────────

    fn collect_candidates_in_block(&self, block: &Block, scan: &mut ElisionScan) {
        for stmt in &block.stmts {
            self.collect_candidates_in_stmt(stmt, scan);
        }
        if let Some(e) = &block.final_expr {
            self.collect_candidates_in_expr(e, scan);
        }
    }

    fn collect_candidates_in_stmt(&self, stmt: &Stmt, scan: &mut ElisionScan) {
        match &stmt.kind {
            StmtKind::Let { pattern, value, .. } => {
                if let PatternKind::Binding(name) = &pattern.kind {
                    let rebound = !scan.bound_names.insert(name.clone());
                    if rebound {
                        scan.block(name, "name bound more than once", &pattern.span);
                    } else if let ExprKind::StructLiteral { path, .. } = &value.kind {
                        if let Some(struct_name) = path.last() {
                            if self.elision_eligible_struct(struct_name) {
                                scan.candidates.insert(name.clone(), struct_name.clone());
                            }
                        }
                    }
                } else {
                    for n in pattern.binding_names() {
                        if !scan.bound_names.insert(n.clone()) {
                            scan.block(&n, "name bound more than once", &pattern.span);
                        }
                    }
                }
                self.collect_candidates_in_expr(value, scan);
            }
            StmtKind::LetUninit { name, .. } => {
                if !scan.bound_names.insert(name.clone()) {
                    scan.block(name, "name bound more than once", &stmt.span);
                }
            }
            StmtKind::LetElse {
                pattern,
                value,
                else_block,
                ..
            } => {
                for n in pattern.binding_names() {
                    if !scan.bound_names.insert(n.clone()) {
                        scan.block(&n, "name bound more than once", &pattern.span);
                    }
                }
                self.collect_candidates_in_expr(value, scan);
                self.collect_candidates_in_block(else_block, scan);
            }
            StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                self.collect_candidates_in_block(body, scan);
            }
            StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
                self.collect_candidates_in_expr(target, scan);
                self.collect_candidates_in_expr(value, scan);
            }
            StmtKind::Expr(e) => self.collect_candidates_in_expr(e, scan),
        }
    }

    /// Candidate collection only needs to find nested LETS (inside
    /// blocks / control flow) and record pattern-bound names; it
    /// doesn't classify uses (pass 2 does). Unknown constructs are
    /// fine here — pass 2's catch-all poisons them.
    fn collect_candidates_in_expr(&self, expr: &Expr, scan: &mut ElisionScan) {
        match &expr.kind {
            ExprKind::Block(b)
            | ExprKind::Seq(b)
            | ExprKind::Unsafe(b)
            | ExprKind::Try(b)
            | ExprKind::Par(b)
            | ExprKind::Loop { body: b, .. }
            | ExprKind::While { body: b, .. }
            | ExprKind::LabeledBlock { body: b, .. } => self.collect_candidates_in_block(b, scan),
            ExprKind::If {
                then_block,
                else_branch,
                ..
            } => {
                self.collect_candidates_in_block(then_block, scan);
                if let Some(e) = else_branch {
                    self.collect_candidates_in_expr(e, scan);
                }
            }
            ExprKind::IfLet {
                pattern,
                then_block,
                else_branch,
                ..
            } => {
                for n in pattern.binding_names() {
                    if !scan.bound_names.insert(n.clone()) {
                        scan.block(&n, "name bound more than once", &pattern.span);
                    }
                }
                self.collect_candidates_in_block(then_block, scan);
                if let Some(e) = else_branch {
                    self.collect_candidates_in_expr(e, scan);
                }
            }
            ExprKind::WhileLet {
                pattern, body: b, ..
            } => {
                for n in pattern.binding_names() {
                    if !scan.bound_names.insert(n.clone()) {
                        scan.block(&n, "name bound more than once", &pattern.span);
                    }
                }
                self.collect_candidates_in_block(b, scan);
            }
            ExprKind::For {
                pattern, body: b, ..
            } => {
                for n in pattern.binding_names() {
                    if !scan.bound_names.insert(n.clone()) {
                        scan.block(&n, "name bound more than once", &pattern.span);
                    }
                }
                self.collect_candidates_in_block(b, scan);
            }
            ExprKind::Match { arms, .. } => {
                for arm in arms {
                    for n in arm.pattern.binding_names() {
                        if !scan.bound_names.insert(n.clone()) {
                            scan.block(&n, "name bound more than once", &arm.pattern.span);
                        }
                    }
                    self.collect_candidates_in_expr(&arm.body, scan);
                }
            }
            ExprKind::Closure { body, .. } => self.collect_candidates_in_expr(body, scan),
            ExprKind::Lock { body, .. } => self.collect_candidates_in_block(body, scan),
            _ => {}
        }
    }

    // ── Pass 2: use classification (default-deny) ───────────────

    fn scan_block(&self, block: &Block, ctx: Ctx, scan: &mut ElisionScan) {
        for stmt in &block.stmts {
            self.scan_stmt(stmt, ctx, scan);
        }
        if let Some(e) = &block.final_expr {
            self.scan_expr(e, ctx, scan);
        }
    }

    fn scan_stmt(&self, stmt: &Stmt, ctx: Ctx, scan: &mut ElisionScan) {
        match &stmt.kind {
            StmtKind::Let { value, .. } | StmtKind::LetElse { value, .. } => {
                // The candidate's own birth literal carries no candidate
                // mentions (it doesn't exist yet, and eligible structs
                // have primitive fields). Any OTHER let whose RHS
                // mentions a candidate hits the default-deny rules in
                // scan_expr (a bare-identifier RHS is an alias → block).
                self.scan_expr(value, ctx, scan);
                if let StmtKind::LetElse { else_block, .. } = &stmt.kind {
                    self.scan_block(else_block, ctx, scan);
                }
            }
            StmtKind::LetUninit { .. } => {}
            StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                // Defer bodies run at scope exit — same scope, but the
                // ordering interplay with the elided free is untested
                // territory; conservatively poison candidates mentioned
                // inside. Treat like a closure for mention purposes.
                self.scan_block(
                    body,
                    Ctx {
                        in_closure: true,
                        ..ctx
                    },
                    scan,
                );
            }
            StmtKind::Assign { target, value } => {
                self.scan_assign_target(target, ctx, scan);
                self.scan_expr(value, ctx, scan);
            }
            StmtKind::CompoundAssign { target, value, .. } => {
                self.scan_assign_target(target, ctx, scan);
                self.scan_expr(value, ctx, scan);
            }
            StmtKind::Expr(e) => self.scan_expr(e, ctx, scan),
        }
    }

    /// Assignment targets: `x.field = v` on a candidate is an allowed
    /// primitive-field write. A bare `x = v` target is a reassignment
    /// → block. Any other target shape scans generically.
    fn scan_assign_target(&self, target: &Expr, ctx: Ctx, scan: &mut ElisionScan) {
        match &target.kind {
            ExprKind::FieldAccess { object, .. } => {
                if let ExprKind::Identifier(n) = &object.kind {
                    if scan.candidates.contains_key(n.as_str()) {
                        if ctx.in_closure || ctx.in_par {
                            scan.block(n, "used inside closure or par region", &object.span);
                        }
                        // allowed: primitive field write
                        return;
                    }
                }
                self.scan_expr(target, ctx, scan);
            }
            ExprKind::Identifier(n) => {
                scan.block(n, "reassigned", &target.span);
            }
            _ => self.scan_expr(target, ctx, scan),
        }
    }

    fn scan_expr(&self, expr: &Expr, ctx: Ctx, scan: &mut ElisionScan) {
        match &expr.kind {
            // ── leaves with no candidate exposure ──
            ExprKind::Integer(..)
            | ExprKind::Float(..)
            | ExprKind::CharLit(_)
            | ExprKind::ByteLit(_)
            | ExprKind::StringLit(_)
            | ExprKind::MultiStringLit(_)
            | ExprKind::CStringLit { .. }
            | ExprKind::Bool(_)
            | ExprKind::SelfValue
            | ExprKind::SelfType
            | ExprKind::PipePlaceholder
            | ExprKind::Continue { .. }
            | ExprKind::OffsetOf { .. }
            | ExprKind::Error => {}

            // ── the default-deny core ──
            ExprKind::Identifier(n) => {
                if ctx.in_closure || ctx.in_par {
                    scan.block(n, "used inside closure or par region", &expr.span);
                } else {
                    scan.block(n, "aliased, stored, returned, or escaped", &expr.span);
                }
            }
            ExprKind::Path { .. } => {
                // Multi-segment paths are type/assoc references, never
                // local bindings; single-segment paths of a candidate
                // name don't occur (the parser produces Identifier).
            }

            // ── allowed shape 1: field read ──
            ExprKind::FieldAccess { object, .. } => {
                if let ExprKind::Identifier(n) = &object.kind {
                    if scan.candidates.contains_key(n.as_str()) {
                        if ctx.in_closure || ctx.in_par {
                            scan.block(n, "used inside closure or par region", &object.span);
                        }
                        return; // allowed: primitive field read
                    }
                }
                self.scan_expr(object, ctx, scan);
            }

            // ── allowed shape 3: ref-mode call args ──
            ExprKind::Call { callee, args } => {
                let fn_key: Option<String> = match &callee.kind {
                    ExprKind::Identifier(n) => Some(n.clone()),
                    ExprKind::Path { segments, .. } if segments.len() == 1 => {
                        Some(segments[0].clone())
                    }
                    ExprKind::Path { segments, .. } if segments.len() == 2 => {
                        Some(format!("{}.{}", segments[0], segments[1]))
                    }
                    _ => None,
                };
                // Callee position itself can't mention a candidate
                // except via exotic shapes — scan it unless it's a
                // plain name/path.
                if fn_key.is_none() {
                    self.scan_expr(callee, ctx, scan);
                }
                for (i, a) in args.iter().enumerate() {
                    if let ExprKind::Identifier(n) = &a.value.kind {
                        if scan.candidates.contains_key(n.as_str()) {
                            if ctx.in_closure || ctx.in_par {
                                scan.block(n, "used inside closure or par region", &a.value.span);
                                continue;
                            }
                            let mode = fn_key.as_deref().and_then(|k| {
                                self.param_modes
                                    .get(k)
                                    .and_then(|ms| ms.get(i))
                                    .map(|(_, m)| m.clone())
                            });
                            match mode {
                                Some(OwnershipMode::Ref) | Some(OwnershipMode::MutRef) => {
                                    // allowed: borrowed arg
                                }
                                _ => scan.block(
                                    n,
                                    "passed as owned (or unresolved) call argument",
                                    &a.value.span,
                                ),
                            }
                            continue;
                        }
                    }
                    self.scan_expr(&a.value, ctx, scan);
                }
            }

            // ── allowed shape 4: ref-self method receiver ──
            ExprKind::MethodCall {
                object,
                method,
                args,
                ..
            } => {
                if let ExprKind::Identifier(n) = &object.kind {
                    if let Some(struct_name) = scan.candidates.get(n.as_str()).cloned() {
                        if ctx.in_closure || ctx.in_par {
                            scan.block(n, "used inside closure or par region", &object.span);
                        } else {
                            let key = format!("{}.{}", struct_name, method);
                            match self.method_self_modes.get(&key) {
                                Some(SelfParam::Ref) | Some(SelfParam::MutRef) => {
                                    // allowed: borrowed receiver
                                }
                                _ => scan.block(
                                    n,
                                    "receiver of an owned-self (or unresolved) method",
                                    &object.span,
                                ),
                            }
                        }
                        for a in args {
                            self.scan_expr(&a.value, ctx, scan);
                        }
                        return;
                    }
                }
                self.scan_expr(object, ctx, scan);
                for a in args {
                    self.scan_expr(&a.value, ctx, scan);
                }
            }

            // ── blanket boundaries ──
            ExprKind::Closure { body, .. } => {
                self.scan_expr(
                    body,
                    Ctx {
                        in_closure: true,
                        ..ctx
                    },
                    scan,
                );
            }
            ExprKind::Par(b) => {
                self.scan_block(
                    b,
                    Ctx {
                        in_par: true,
                        ..ctx
                    },
                    scan,
                );
            }

            // ── generic recursion ──
            ExprKind::InterpolatedStringLit(parts) => {
                for p in parts {
                    if let ParsedInterpolationPart::Expr(e) = p {
                        self.scan_expr(e, ctx, scan);
                    }
                }
            }
            ExprKind::Binary { left, right, .. } => {
                self.scan_expr(left, ctx, scan);
                self.scan_expr(right, ctx, scan);
            }
            ExprKind::Unary { operand, .. } => self.scan_expr(operand, ctx, scan),
            ExprKind::Question(e) => self.scan_expr(e, ctx, scan),
            ExprKind::OptionalChain { object, .. } => self.scan_expr(object, ctx, scan),
            ExprKind::NilCoalesce { left, right } => {
                self.scan_expr(left, ctx, scan);
                self.scan_expr(right, ctx, scan);
            }
            ExprKind::TupleIndex { object, .. } => self.scan_expr(object, ctx, scan),
            ExprKind::Index { object, index } => {
                self.scan_expr(object, ctx, scan);
                self.scan_expr(index, ctx, scan);
            }
            ExprKind::Block(b) | ExprKind::Seq(b) | ExprKind::Unsafe(b) | ExprKind::Try(b) => {
                self.scan_block(b, ctx, scan);
            }
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                self.scan_expr(condition, ctx, scan);
                self.scan_block(then_block, ctx, scan);
                if let Some(e) = else_branch {
                    self.scan_expr(e, ctx, scan);
                }
            }
            ExprKind::IfLet {
                value,
                then_block,
                else_branch,
                ..
            } => {
                self.scan_expr(value, ctx, scan);
                self.scan_block(then_block, ctx, scan);
                if let Some(e) = else_branch {
                    self.scan_expr(e, ctx, scan);
                }
            }
            ExprKind::Match { scrutinee, arms } => {
                self.scan_expr(scrutinee, ctx, scan);
                for arm in arms {
                    if let Some(g) = &arm.guard {
                        self.scan_expr(g, ctx, scan);
                    }
                    self.scan_expr(&arm.body, ctx, scan);
                }
            }
            ExprKind::While {
                condition, body, ..
            } => {
                self.scan_expr(condition, ctx, scan);
                self.scan_block(body, ctx, scan);
            }
            ExprKind::WhileLet { value, body, .. } => {
                self.scan_expr(value, ctx, scan);
                self.scan_block(body, ctx, scan);
            }
            ExprKind::For { iterable, body, .. } => {
                self.scan_expr(iterable, ctx, scan);
                self.scan_block(body, ctx, scan);
            }
            ExprKind::Loop { body, .. } => self.scan_block(body, ctx, scan),
            ExprKind::LabeledBlock { body, .. } => self.scan_block(body, ctx, scan),
            ExprKind::Return(e) => {
                if let Some(e) = e {
                    self.scan_expr(e, ctx, scan);
                }
            }
            ExprKind::Break { value, .. } => {
                if let Some(e) = value {
                    self.scan_expr(e, ctx, scan);
                }
            }
            ExprKind::Tuple(es) | ExprKind::ArrayLiteral(es) => {
                for e in es {
                    self.scan_expr(e, ctx, scan);
                }
            }
            ExprKind::PrefixCollectionLiteral { items, .. } => {
                for e in items {
                    self.scan_expr(e, ctx, scan);
                }
            }
            ExprKind::RepeatLiteral { value, count, .. } => {
                self.scan_expr(value, ctx, scan);
                self.scan_expr(count, ctx, scan);
            }
            ExprKind::MapLiteral(pairs) => {
                for (k, v) in pairs {
                    self.scan_expr(k, ctx, scan);
                    self.scan_expr(v, ctx, scan);
                }
            }
            ExprKind::StructLiteral { fields, .. } => {
                for f in fields {
                    self.scan_expr(&f.value, ctx, scan);
                }
            }
            ExprKind::Pipe { left, right } => {
                self.scan_expr(left, ctx, scan);
                self.scan_expr(right, ctx, scan);
            }
            ExprKind::Cast { expr: inner, .. } => self.scan_expr(inner, ctx, scan),
            ExprKind::Range { start, end, .. } => {
                if let Some(s) = start {
                    self.scan_expr(s, ctx, scan);
                }
                if let Some(e) = end {
                    self.scan_expr(e, ctx, scan);
                }
            }
            ExprKind::Lock { body, .. } => {
                // Lock bodies execute inline but guard concurrent state;
                // a candidate mentioned inside is at minimum adjacent to
                // cross-task data. Conservatively treat as a par region.
                self.scan_block(
                    body,
                    Ctx {
                        in_par: true,
                        ..ctx
                    },
                    scan,
                );
            }
            ExprKind::Providers { .. } => {
                scan.poison_all("unhandled construct (providers)", &expr.span);
            }
        }
    }
}
