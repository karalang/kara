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

        // Phase B1: cluster discovery (separate walk; phase-A
        // candidates are all-primitive types and cluster members carry
        // a link field, so the two sets are disjoint by construction).
        let mut clusters: HashMap<String, Vec<ElidedCluster>> = HashMap::new();
        for item in &self.program.items {
            match item {
                Item::Function(f) => {
                    let mut cs = self.fn_clusters(f);
                    // Phase C2a: borrowed-param walk families are
                    // callee-local (no cross-fn dependency) — computed
                    // in the same walk. Free fns only in v1 (the
                    // C2b call-site contract keys on bare-Identifier
                    // callees).
                    cs.extend(self.fn_borrowed_families(f));
                    if !cs.is_empty() {
                        clusters.insert(f.name.clone(), cs);
                    }
                }
                Item::ImplBlock(imp) => {
                    let type_name = match &imp.target_type.kind {
                        TypeKind::Path(p) => p.segments.last().cloned().unwrap_or_default(),
                        _ => continue,
                    };
                    for item in &imp.items {
                        if let ImplItem::Method(method) = item {
                            let cs = self.fn_clusters(method);
                            if !cs.is_empty() {
                                clusters.insert(format!("{}.{}", type_name, method.name), cs);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        // Phase C1c: caller adoption. With every builder's fresh-return
        // summary known (`returned != No` ⇒ the call result is a chain
        // at rc==1 per node), a second walk grows ADOPTED clusters
        // around builder-call results in caller fns. Free-fn builders
        // only (the candidate callee is a bare Identifier); method
        // callers participate like free fns.
        let mut builder_summaries: HashMap<String, (String, usize)> = HashMap::new();
        for (fn_key, cs) in &clusters {
            for c in cs {
                if c.returned != ReturnedChain::No {
                    builder_summaries
                        .insert(fn_key.clone(), (c.member_type.clone(), c.link_field_index));
                }
            }
        }
        // Phase C2a/C2b: borrowed-position summaries (fn → (member
        // type, positions)) feed the adopted scans' sanctioned-arg
        // channel.
        let mut borrow_summaries: HashMap<String, (String, HashSet<usize>)> = HashMap::new();
        for (fn_key, cs) in &clusters {
            for c in cs {
                if c.borrowed {
                    borrow_summaries.insert(
                        fn_key.clone(),
                        (
                            c.member_type.clone(),
                            c.borrowed_params.iter().map(|(_, i)| *i).collect(),
                        ),
                    );
                }
            }
        }
        if !builder_summaries.is_empty() {
            for item in &self.program.items {
                match item {
                    Item::Function(f) => {
                        let acs =
                            self.fn_adopted_clusters(f, &builder_summaries, &borrow_summaries);
                        if !acs.is_empty() {
                            clusters.entry(f.name.clone()).or_default().extend(acs);
                        }
                    }
                    Item::ImplBlock(imp) => {
                        let type_name = match &imp.target_type.kind {
                            TypeKind::Path(p) => p.segments.last().cloned().unwrap_or_default(),
                            _ => continue,
                        };
                        for item in &imp.items {
                            if let ImplItem::Method(method) = item {
                                let acs = self.fn_adopted_clusters(
                                    method,
                                    &builder_summaries,
                                    &borrow_summaries,
                                );
                                if !acs.is_empty() {
                                    clusters
                                        .entry(format!("{}.{}", type_name, method.name))
                                        .or_default()
                                        .extend(acs);
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        // Phase C2b: the program-wide headerless-T gate, then the
        // post-filter — adopted families that used the sanctioned-arg
        // channel are sound ONLY under headerless-T, so they
        // deactivate (fall back to full RC) for types that failed.
        let headerless_types =
            self.compute_headerless_types(&clusters, &builder_summaries, &borrow_summaries);
        for cs in clusters.values_mut() {
            cs.retain(|c| !c.arg_sanctioned || headerless_types.contains_key(&c.member_type));
        }
        self.headerless_types = headerless_types;
        self.elided_clusters = clusters;
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
            StmtKind::MultiAssign { .. } => unreachable!(
                "StmtKind::MultiAssign is removed by the desugar pass before reaching this phase"
            ),
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
            StmtKind::MultiAssign { .. } => unreachable!(
                "StmtKind::MultiAssign is removed by the desugar pass before reaching this phase"
            ),
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

// ════════════════════════════════════════════════════════════════
// Phase B1 — local cluster elision, append-only chain shape.
// ════════════════════════════════════════════════════════════════
//
// Widens the elision unit from a single binding to a *cluster*: a
// self-linked chain built and dropped inside one function. B1 consumes
// the analysis on the DROP side only — the cluster ROOT's scope-exit
// cleanup becomes a link-following free-walk (`FreeClusterWalk`: load
// next, free, advance — no dec, no zero-test, no drop-fn dispatch).
// All build-time count traffic stays untouched, so the existing
// (suite- and ASAN-proven) discipline keeps every intermediate state
// correct; the only new obligation is the free-walk's precondition.
//
// ## Soundness argument
//
// The free-walk frees every node reachable from the root while
// ignoring refcounts. That is sound iff, at drain time, each reachable
// node's rc equals its parent-link count and that count is exactly 1:
//
// 1. **rc == #parents at drain.** Build traffic is unchanged and
//    balanced (today's dec-walk frees these exact shapes leak- and
//    UAF-free under ASAN — that IS the proof that rc == #owners at
//    scope exit). Cursor bindings hold +1 refs, but cleanup frames
//    drain LIFO and the root is required to be the FIRST-declared
//    cluster binding, so every cursor's RcDec runs BEFORE the root's
//    free-walk — after them, owners = parent links only.
// 2. **#parents == 1 (append-only).** Each fresh node may appear in
//    link-VALUE position (`cursor.link = Some(node)`) at most once
//    syntactically, fresh-node literals carry `link: None`, and
//    cursors may never appear in link-value position — so no node can
//    ever acquire a second parent, and link overwrites merely orphan
//    the displaced node (the build traffic's release-old already frees
//    it; it is then unreachable from the root). The PREPEND idiom
//    (`let n = T { link: head }; head = Some(n);`) is deliberately
//    NOT covered: its soundness couples the literal-init link to the
//    immediately-following root reassignment (flow-sensitive) — B1.1
//    territory, blocked by the literal-link-init rule below.
//
// ## v1 shape rules (default-deny, whole-cluster poisoning)
//
// - member type: `shared struct` (non-par, no user Drop) with exactly
//   one `Option[Self]` link field; every other field primitive.
// - root: the first-declared cluster binding; must be
//   `let r = T { ..., link: None };` (bare literal root).
// - fresh nodes: `let n = T { ..., link: None };`
// - bare cursors: `let/assign c = <bare cluster ident>;` plus
//   `let/assign c = <option cursor>.unwrap();`
// - option cursors: `let/assign oc = <bare cluster ident>.link;`
// - link stores: `<bare cluster ident>.link = Some(<fresh node>)`
//   (or `= None`); each fresh node in at most ONE such site.
// - `is_some()` / `is_none()` on option cursors: anywhere.
// - primitive field reads/writes on bare bindings: anywhere.
// - member-type PARAMS coexist (C1a): they never poison by presence —
//   the rules above wall them out of the cluster from both sides
//   (they can't join membership, can't be link-stored [non-fresh],
//   and a fresh node stored under a param hits default-deny). Param
//   values keep full RC; a param name colliding with a cluster name
//   poisons via the shadow check.
// - EVERYTHING else mentioning a cluster name blocks the whole
//   cluster: calls, method receivers/args, returns/tails, `Some(x)`
//   outside a link store, match/if-let, closures, par/lock regions,
//   comparisons, stores into non-cluster aggregates, root
//   reassignment. Unknown constructs poison (same discipline as
//   phase A).

/// Phase C1b: how a cluster's chain leaves its function. Both
/// returning forms are sound ONLY under `b2` — the count-free build
/// means every node leaves the builder at rc==1 straight from
/// `rc_alloc`, so the caller's ordinary dec-drop (or a future
/// C1c free-walk adoption) composes without any compensation. A
/// B1-only cluster with a sanctioned return shape is therefore not a
/// cluster at all (the escape stands; full RC).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReturnedChain {
    /// Not returned — the root free-walks (B1) at scope exit.
    No,
    /// Function final expr is `<root>.<link>` — the dummy-header
    /// builder (kata #2's `add_two_numbers` returns `dummy.next`).
    /// The root header node frees ALONE at scope exit; the chain
    /// transfers out through the loaded link at rc==1 per node.
    RootLink,
    /// Function final expr is `Some(<root>)` — the bare-root builder
    /// (kata #2's `from_array`). No root cleanup at all; the entire
    /// cluster transfers to the caller.
    SomeRoot,
}

/// A phase-B1 cluster eligible for the root free-walk. `bindings`
/// records the full cluster-local name set (root + fresh nodes +
/// cursors) — B1's codegen only consumes `root`/`member_type`/
/// `link_field_index`; the set is surfaced for tests and for phase
/// B2's build-side elision.
#[derive(Debug, Clone)]
pub struct ElidedCluster {
    pub root: String,
    pub member_type: String,
    /// User-field index (declaration order, refcount header excluded)
    /// of the `Option[Self]` link field — codegen GEPs heap index
    /// `link_field_index + 1`.
    pub link_field_index: usize,
    pub bindings: HashSet<String>,
    /// Phase B2: build-side count-op elision approved. True only for
    /// the displacement-free shape (see `recognize_b2`): exactly one
    /// link-store site, either outside every loop or the canonical
    /// adjacent append triple (`let node = T{..., link: None};
    /// cursor.link = Some(node); cursor = node;`), every link READ
    /// strictly after the store region, and no never-linked fresh
    /// nodes. Under those rules NOTHING is freed before the root's
    /// scope-exit free-walk, so non-owning (count-free) cursors can
    /// never dangle and the elided link store is a pure pointer store.
    pub b2: bool,
    /// B2 roles (meaningful only when `b2`): the fresh node name(s)
    /// consumed by the link store. No count ops, no cleanup — their
    /// object is owned by the chain.
    pub fresh_linked: HashSet<String>,
    /// B2 roles: bare `T` cursors (aliases). No count ops, no cleanup.
    pub bare_cursors: HashSet<String>,
    /// B2 roles: `Option[T]` link-read cursors. No count ops, no
    /// `RcDecOption`, plain-store reassignment.
    pub option_cursors: HashSet<String>,
    /// Phase D: headerless member layout approved. True only when `b2`
    /// holds AND the member type passes the dual purity gate: (a)
    /// fn-level — no free (non-cluster-let) member literals, no
    /// boundary regions (closure/par/lock/defer) anywhere in the fn,
    /// no type annotation mentioning the member type; (b) program-
    /// level — no other declared type/signature/alias in the program
    /// mentions the member type, so no headered `T` value can ever
    /// enter (or leave) this function. Under the gate, codegen may key
    /// the heap layout per `(fn, member_type)`: members are allocated
    /// WITHOUT the 8-byte rc header (`malloc(size - 8)`, field GEPs at
    /// user index instead of `idx + 1`) and the root's free-walk geps
    /// the shifted link slot. b2 is a structural precondition, not an
    /// optimization: a headerless node has no rc word, so any count op
    /// that slipped through would corrupt the first field.
    pub headerless: bool,
    /// Phase C1b fresh-return summary — see `ReturnedChain`. Non-`No`
    /// only when `b2` (count-free build is the precondition for the
    /// structural rc==1 transfer). A returned cluster is never
    /// `headerless` (the chain crosses the fn boundary headered).
    pub returned: ReturnedChain,
    /// Phase C1c caller adoption: this cluster's root is NOT a fresh
    /// literal but the result of a call to a fresh-return builder
    /// (`returned != No` in the callee). The chain arrives at rc==1
    /// per node (the builder's b2 count-free build), the family is
    /// read-only by construction (no fresh nodes exist, so every link
    /// store poisons as non-fresh — nothing is ever freed or displaced
    /// mid-scope), and therefore cursors are non-owning exactly as in
    /// b2 (`b2` is set on adopted clusters so codegen reuses the same
    /// count-skip roles). The root's scope-exit cleanup is an
    /// Option-tag-guarded `FreeClusterWalk` over the whole chain
    /// instead of the recursive dec-walk. Adopted clusters are never
    /// `headerless` and never `returned`.
    pub adopted: bool,
    /// Phase C2a borrowed-param family: the family is rooted at the
    /// fn's bare-owned `Option[T]` PARAMS (all of them — one family
    /// per (fn, T), so kata #2's `if let Some(n) = a` / `= b` pair
    /// shares the pattern-bound `n`). The params themselves KEEP their
    /// balanced entry/exit ownership (caller incs the head at the call
    /// site, the callee's `RcDecOption` decs it at exit) — only the
    /// WALK traffic (aliases, cursors, pattern binds, advances) is
    /// count-free via the b2 roles. Read-only like adopted families;
    /// nothing is freed mid-scope, so non-owning cursors never dangle,
    /// and the family queues NO cleanup of its own. The two-sided
    /// skip of the residual head counts (call-site inc + exit dec) is
    /// C2b's: it requires the program-purity gate (incl. a
    /// fn-referenced-as-value scan) that headerless-T needs anyway.
    pub borrowed: bool,
    /// The borrowed params as `(name, 0-based position)` — C2b's
    /// contract surface: codegen keys the callee-side exit-dec skip by
    /// name and the call-site inc skip by position (both gated on the
    /// member type being program-wide headerless).
    pub borrowed_params: Vec<(String, usize)>,
    /// Phase C2b fn-purity bit consumed by the headerless-T coverage
    /// check. Literal clusters: no free member literals AND no
    /// boundary regions in the fn (the two layout hazards a cluster
    /// scan can see). Adopted/borrowed families: no boundary regions
    /// only — their scans mis-flag sibling literal clusters' literals
    /// as free (foreign-classified), so the literal rule is owned by
    /// the literal cluster's own flag.
    pub fn_pure: bool,
    /// Phase C2b: this ADOPTED family used the sanctioned-arg channel
    /// (a member passed at a borrowed position of a summarized
    /// callee). Sound ONLY under program-wide headerless-T (both
    /// residual-count skips active); when T fails the gate — in
    /// analysis OR in codegen's coro/niche reconcile — the family
    /// deactivates and the binding falls back to full RC (registered
    /// let → arg-site inc → callee exit dec, today's balance).
    pub arg_sanctioned: bool,
}

/// Cluster-binding role during the scan.
#[derive(Clone, Copy, PartialEq)]
enum ClusterKind {
    /// Bare `T` handle: root, fresh node, or bare cursor.
    Bare,
    /// `Option[T]` handle: link-read cursor.
    OptionCursor,
}

struct ClusterScan {
    member_type: String,
    link_field: String,
    /// name → kind for every cluster-local binding.
    bindings: HashMap<String, ClusterKind>,
    /// Fresh-node binding names (`let n = T { ..., link: None };`) in
    /// declaration order; each may take at most one link-value slot.
    fresh: HashSet<String>,
    /// Fresh names already consumed by a link-value site.
    linked_once: HashSet<String>,
    root: Option<String>,
    /// Whether any fresh node was actually linked (a cluster with no
    /// links gains nothing over per-binding phase A — skip it).
    any_link: bool,
    /// Names bound by NON-let patterns anywhere in the fn (for/match/
    /// if-let/while-let/let-else patterns, closure params). A cluster
    /// name colliding with any of these is shadowed somewhere — the
    /// name-keyed analysis could then misattribute an external object
    /// to the cluster (e.g. link an externally-referenced node into
    /// the root's chain through a shadowed fresh name), so the whole
    /// cluster poisons on intersection.
    shadow_names: HashSet<String>,
    poisoned: Option<(String, Span)>,
    /// Phase D fn-purity flags (demote headerless, keep B1/B2). Set
    /// during the verify walk — it is the complete default-deny
    /// enumeration, so piggybacking here costs no new walker.
    /// A member-type struct literal in any position other than a
    /// cluster-let RHS (those return early in `cluster_verify_stmt`;
    /// assign/link-store literal shapes poison outright).
    free_member_literal: bool,
    /// Any closure / par / lock / defer region exists in the fn — its
    /// body compiles under a different fn key, so a member literal or
    /// layout-sensitive access inside would disagree with the outer
    /// fn's per-(fn, type) layout decision.
    saw_boundary_region: bool,
    /// Some `let` / `let-else` / uninit annotation mentions the member
    /// type — an annotated non-cluster binding of `T` (or a container
    /// of `T`) could hold a headered value the per-type keying would
    /// then mis-GEP.
    annotation_mentions_member: bool,
    /// Phase C1c: `Some((root_name, builder_fn))` when this scan grows
    /// an ADOPTED family around a fresh-return builder call result
    /// instead of a literal root. Gates the adoption-only membership
    /// shapes: the candidate's own `let <root> = <builder>(...)`,
    /// option-cursor aliasing (`let cur = <option member>`), and the
    /// sanctioned read-only `match` (`match <option member> {
    /// Some(<binding>) => ..., None/_ => ... }`). `None` = literal
    /// cluster scan (B1/B2 behavior unchanged).
    adopted_root: Option<(String, String)>,
    /// Phase C2a: this scan grows a BORROWED-PARAM family (roots are
    /// pre-seeded param names; see `ElidedCluster::borrowed`). Shares
    /// the family-only membership shapes with adopted scans, plus:
    /// member-type literals classify as FOREIGN (the fn's own literal
    /// cluster coexists independently), and the sanctioned
    /// if-let/while-let bind joins the family.
    borrowed: bool,
    /// Phase C2b (adopted scans only): borrowed-position summaries of
    /// every summarized callee — `fn name → {borrowed positions}` for
    /// the scan's member type. A family OPTION member passed at such a
    /// position is the sanctioned-arg channel (the callee borrows; the
    /// caller-side inc and callee-side exit dec are both skipped under
    /// headerless-T). Empty for literal/borrowed scans.
    arg_borrow_positions: HashMap<String, HashSet<usize>>,
    /// Set when the sanctioned-arg channel fired at least once.
    used_arg_sanction: bool,
    /// Names bound by SANCTIONED match/if-let/while-let patterns —
    /// the one re-bind exception: kata #2 binds `n` in two sibling
    /// if-lets (one per param chain). A pattern member may be re-bound
    /// by another sanctioned pattern (each bind is body-scoped and
    /// count-neutral in codegen); collision with a LET-born member
    /// still poisons (the name-keyed analysis can't scope those).
    pattern_members: HashSet<String>,
}

impl ClusterScan {
    /// Family scans (adopted call results / borrowed params) share the
    /// non-literal membership shapes — option-cursor aliasing and the
    /// sanctioned read-only pattern binds — and the read-only rule
    /// (every link store poisons).
    fn is_family(&self) -> bool {
        self.adopted_root.is_some() || self.borrowed
    }

    /// Join a sanctioned-pattern bound name as a Bare member. Re-binds
    /// are allowed ONLY between sanctioned patterns (each bind is
    /// body-scoped; codegen's pattern binding is count-neutral);
    /// collision with a let-born member poisons — the name-keyed
    /// analysis cannot scope that shadowing.
    fn insert_pattern_member(&mut self, name: String, span: &Span) {
        if self.bindings.contains_key(name.as_str()) {
            if !self.pattern_members.contains(name.as_str()) {
                self.poison("cluster name bound more than once", span);
            }
            return;
        }
        self.pattern_members.insert(name.clone());
        self.bindings.insert(name, ClusterKind::Bare);
    }
}

impl ClusterScan {
    fn poison(&mut self, reason: &str, span: &Span) {
        if self.poisoned.is_none() {
            self.poisoned = Some((reason.to_string(), span.clone()));
        }
    }

    fn kind_of(&self, name: &str) -> Option<ClusterKind> {
        self.bindings.get(name).copied()
    }
}

impl<'a> OwnershipChecker<'a> {
    /// True when `name` is a B1-eligible chain link type; returns the
    /// link field's name and user index.
    fn cluster_link_struct(&self, name: &str) -> Option<(String, usize)> {
        let info = self.typecheck_result.struct_info.get(name)?;
        if !info.is_shared || info.is_par {
            return None;
        }
        if self.typecheck_result.drop_method_keys.contains_key(name) {
            return None;
        }
        let mut link: Option<(String, usize)> = None;
        for (idx, (fname, ty, _)) in info.fields.iter().enumerate() {
            match ty {
                Type::Int(_) | Type::UInt(_) | Type::Float(_) | Type::Bool | Type::Char => {}
                Type::Named { name: n, args } if n == "Option" && args.len() == 1 => {
                    match &args[0] {
                        Type::Shared(inner) if inner == name => {
                            if link.is_some() {
                                return None; // multi-link → not v1
                            }
                            link = Some((fname.clone(), idx));
                        }
                        _ => return None,
                    }
                }
                _ => return None,
            }
        }
        link
    }

    /// B1 driver for one function: discover at most one cluster per
    /// member type, verify the append-only rules, and return the
    /// clusters whose roots may take the free-walk.
    pub(crate) fn fn_clusters(&self, f: &Function) -> Vec<ElidedCluster> {
        // Collect candidate member types from struct literals in this
        // fn (cheap pre-pass: a fn without a T-literal can't host a
        // T-cluster).
        let mut member_types: Vec<(String, String, usize)> = Vec::new();
        let mut seen = HashSet::new();
        collect_struct_literal_types(&f.body, &mut |type_name| {
            if seen.insert(type_name.to_string()) {
                if let Some((field, idx)) = self.cluster_link_struct(type_name) {
                    member_types.push((type_name.to_string(), field, idx));
                }
            }
        });
        let mut out = Vec::new();
        for (member, link_field, link_idx) in member_types {
            let mut scan = ClusterScan {
                member_type: member.clone(),
                link_field: link_field.clone(),
                bindings: HashMap::new(),
                fresh: HashSet::new(),
                linked_once: HashSet::new(),
                root: None,
                any_link: false,
                shadow_names: HashSet::new(),
                poisoned: None,
                free_member_literal: false,
                saw_boundary_region: false,
                annotation_mentions_member: false,
                adopted_root: None,
                borrowed: false,
                arg_borrow_positions: HashMap::new(),
                used_arg_sanction: false,
                pattern_members: HashSet::new(),
            };
            // Phase C1a: member-type params do NOT poison. The flow
            // walls keep them strictly foreign to the cluster:
            // membership only admits fresh link-None literals /
            // aliases / link-reads / unwraps OF CLUSTER BINDINGS, the
            // link store requires its value ∈ `fresh` (param splice →
            // "non-fresh" poison), a cluster name reaching any other
            // position (incl. `param.link = Some(fresh)`'s RHS) hits
            // the default-deny Identifier arm, and a param name
            // colliding with a cluster name poisons via
            // `shadow_names` below. Phase D demotes automatically: a
            // param of the member type is a signature mention, so
            // `program_leaks_member_type` already forces headered
            // layout program-wide.
            //
            // Pass 1: grow membership from lets, in declaration order.
            self.cluster_collect_block(&f.body, &mut scan);
            // Shadow check: any cluster name also bound by a non-let
            // pattern (or fn param) somewhere in the fn poisons — see
            // `shadow_names`.
            for p in &f.params {
                for n in p.pattern.binding_names() {
                    scan.shadow_names.insert(n);
                }
            }
            if let Some(shadowed) = scan
                .bindings
                .keys()
                .find(|n| scan.shadow_names.contains(n.as_str()))
                .cloned()
            {
                scan.poison(
                    &format!("cluster name '{shadowed}' shadowed by a pattern binding"),
                    &f.span,
                );
            }
            // Phase C1b: detect the sanctioned fresh-return tail shape
            // BEFORE pass 2 (the verify would otherwise poison it as
            // an escape). Only the function body's final expression
            // qualifies — statement-position `return`s of cluster
            // values keep poisoning via the default-deny Identifier
            // arm, so a fn with both a tail return and a mid-fn escape
            // never forms a cluster.
            let mut returned = ReturnedChain::No;
            if let (Some(root), Some(fe)) = (scan.root.as_deref(), f.body.final_expr.as_ref()) {
                returned = match &fe.kind {
                    ExprKind::FieldAccess { object, field }
                        if field == &scan.link_field
                            && matches!(&object.kind, ExprKind::Identifier(n) if n == root) =>
                    {
                        ReturnedChain::RootLink
                    }
                    ExprKind::Call { callee, args }
                        if args.len() == 1
                            && matches!(&callee.kind, ExprKind::Identifier(c) if c == "Some")
                            && matches!(&args[0].value.kind, ExprKind::Identifier(n) if n == root) =>
                    {
                        ReturnedChain::SomeRoot
                    }
                    _ => ReturnedChain::No,
                };
            }
            // Pass 2: verify every use (default-deny). The sanctioned
            // tail expr is skipped — it is the one allowed escape.
            for stmt in &f.body.stmts {
                self.cluster_verify_stmt(stmt, ClusterCtx::default(), &mut scan);
            }
            if returned == ReturnedChain::No {
                if let Some(e) = &f.body.final_expr {
                    self.cluster_verify_expr(e, ClusterCtx::default(), &mut scan);
                }
            }
            if scan.poisoned.is_none() && scan.root.is_some() && scan.any_link {
                let root = scan.root.clone().unwrap();
                let b2_roles = recognize_b2(f, &scan);
                let (b2, fresh_linked) = match &b2_roles {
                    Some(fresh) => (true, fresh.clone()),
                    None => (false, HashSet::new()),
                };
                // C1b soundness gate: a returned chain transfers at
                // rc==1 per node ONLY because the b2 count-free build
                // never inflates the counts. Without b2 the link-store
                // retains would leave rc==2 nodes that the caller's
                // dec-drop leaks — so the escape stands and no cluster
                // forms at all (today's full-RC behavior).
                if returned != ReturnedChain::No && !b2 {
                    continue;
                }
                let mut bare_cursors = HashSet::new();
                let mut option_cursors = HashSet::new();
                if b2 {
                    for (name, kind) in &scan.bindings {
                        if name == &root || fresh_linked.contains(name) {
                            continue;
                        }
                        match kind {
                            ClusterKind::Bare => {
                                bare_cursors.insert(name.clone());
                            }
                            ClusterKind::OptionCursor => {
                                option_cursors.insert(name.clone());
                            }
                        }
                    }
                }
                // Phase D: dual purity gate on top of b2. The fn-level
                // flags were gathered during the verify walk; the
                // program-level scan proves no other declared type /
                // signature / alias mentions the member type (so no
                // headered T can cross this fn's boundary in either
                // direction). Demotion is invisible to B1/B2.
                let headerless = b2
                    && returned == ReturnedChain::No
                    && !scan.free_member_literal
                    && !scan.saw_boundary_region
                    && !scan.annotation_mentions_member
                    && !self.program_leaks_member_type(&member);
                out.push(ElidedCluster {
                    root,
                    member_type: member,
                    link_field_index: link_idx,
                    bindings: scan.bindings.keys().cloned().collect(),
                    b2,
                    fresh_linked,
                    bare_cursors,
                    option_cursors,
                    headerless,
                    returned,
                    adopted: false,
                    borrowed: false,
                    borrowed_params: Vec::new(),
                    fn_pure: !scan.free_member_literal && !scan.saw_boundary_region,
                    arg_sanctioned: false,
                });
            }
        }
        out
    }

    /// Phase C1c driver for one caller function: grow an ADOPTED
    /// cluster around each `let <name> = <builder>(...)` whose callee
    /// has a fresh-return summary. Each candidate gets its own
    /// independent scan (other candidates' names are foreign to it —
    /// the default-deny only fires on the family's own names), so
    /// kata #2's `main` adopts `out` while `l1`/`l2` (passed onward as
    /// call args) are rejected by their own scans and keep full RC.
    ///
    /// Conservative walls:
    /// - a fn that contains ANY member-type literal skips adoption for
    ///   that type (no interplay with literal-cluster discovery, which
    ///   is one-cluster-per-type);
    /// - candidates are only discovered in the plain fn body (not
    ///   inside closure/par/lock/defer bodies — those compile under a
    ///   different fn key, so the let-site cleanup would never fire);
    /// - everything else is the same default-deny verify as B1.
    fn fn_adopted_clusters(
        &self,
        f: &Function,
        summaries: &HashMap<String, (String, usize)>,
        borrow_summaries: &HashMap<String, (String, HashSet<usize>)>,
    ) -> Vec<ElidedCluster> {
        let mut literal_types = HashSet::new();
        collect_struct_literal_types(&f.body, &mut |type_name| {
            literal_types.insert(type_name.to_string());
        });
        let mut candidates: Vec<(String, String)> = Vec::new();
        collect_adoption_candidates(&f.body, summaries, &mut candidates);
        let mut out = Vec::new();
        for (name, builder) in candidates {
            let (member, link_idx) = summaries[&builder].clone();
            if literal_types.contains(&member) {
                continue;
            }
            // The builder's own cluster proved the type's link shape;
            // re-derive the field name for the scan.
            let Some((link_field, _)) = self.cluster_link_struct(&member) else {
                continue;
            };
            // Borrowed positions of summarized callees, filtered to
            // this candidate's member type.
            let arg_borrow_positions: HashMap<String, HashSet<usize>> = borrow_summaries
                .iter()
                .filter(|(_, (t, _))| t == &member)
                .map(|(g, (_, pos))| (g.clone(), pos.clone()))
                .collect();
            let mut scan = ClusterScan {
                member_type: member.clone(),
                link_field,
                bindings: HashMap::new(),
                fresh: HashSet::new(),
                linked_once: HashSet::new(),
                root: None,
                any_link: false,
                shadow_names: HashSet::new(),
                poisoned: None,
                free_member_literal: false,
                saw_boundary_region: false,
                annotation_mentions_member: false,
                adopted_root: Some((name.clone(), builder.clone())),
                borrowed: false,
                arg_borrow_positions: arg_borrow_positions.clone(),
                used_arg_sanction: false,
                pattern_members: HashSet::new(),
            };
            for p in &f.params {
                for n in p.pattern.binding_names() {
                    scan.shadow_names.insert(n);
                }
            }
            // Pass 1: membership (the candidate let anchors the root;
            // derivations join via the cursor rules + the C1c alias /
            // sanctioned-match shapes).
            self.cluster_collect_block(&f.body, &mut scan);
            if let Some(shadowed) = scan
                .bindings
                .keys()
                .find(|n| scan.shadow_names.contains(n.as_str()))
                .cloned()
            {
                scan.poison(
                    &format!("cluster name '{shadowed}' shadowed by a pattern binding"),
                    &f.span,
                );
            }
            // Pass 2: default-deny verify — no C1b tail exemption
            // (adopted chains never leave the caller; a returned /
            // escaping family member poisons and the binding keeps
            // full RC, today's behavior).
            for stmt in &f.body.stmts {
                self.cluster_verify_stmt(stmt, ClusterCtx::default(), &mut scan);
            }
            if let Some(e) = &f.body.final_expr {
                self.cluster_verify_expr(e, ClusterCtx::default(), &mut scan);
            }
            if scan.poisoned.is_none() && scan.root.is_some() {
                let root = scan.root.clone().unwrap();
                let mut bare_cursors = HashSet::new();
                let mut option_cursors = HashSet::new();
                for (n, kind) in &scan.bindings {
                    if n == &root {
                        continue;
                    }
                    match kind {
                        ClusterKind::Bare => {
                            bare_cursors.insert(n.clone());
                        }
                        ClusterKind::OptionCursor => {
                            option_cursors.insert(n.clone());
                        }
                    }
                }
                out.push(ElidedCluster {
                    root,
                    member_type: member,
                    link_field_index: link_idx,
                    bindings: scan.bindings.keys().cloned().collect(),
                    // The family is read-only (no fresh nodes exist, so
                    // every link store poisons as non-fresh): the same
                    // displacement-free argument that makes b2 cursors
                    // count-free applies, and codegen reuses the b2
                    // role machinery for them.
                    b2: true,
                    fresh_linked: HashSet::new(),
                    bare_cursors,
                    option_cursors,
                    headerless: false,
                    returned: ReturnedChain::No,
                    adopted: true,
                    borrowed: false,
                    borrowed_params: Vec::new(),
                    fn_pure: !scan.saw_boundary_region,
                    arg_sanctioned: scan.used_arg_sanction,
                });
            }
        }
        out
    }

    /// Phase C2a driver for one function: grow a BORROWED family per
    /// member type around the fn's bare-owned `Option[T]` params. All
    /// T-params join one family (all-or-nothing: if any param's uses
    /// escape the read-only walk set, the whole family poisons and
    /// every param keeps full RC — exactly today's behavior).
    ///
    /// The params themselves keep their balanced entry/exit ownership
    /// (caller-side head inc, callee exit `RcDecOption`) and are NOT
    /// given count-skip roles — analysis may even allow `l1 = a`
    /// cursor re-aims on them, which codegen handles through the
    /// registered full-RC reassign path. Only the derived walk
    /// bindings (aliases / cursors / pattern binds) go count-free.
    fn fn_borrowed_families(&self, f: &Function) -> Vec<ElidedCluster> {
        // Group eligible params by member type.
        let mut by_type: HashMap<String, Vec<(String, usize)>> = HashMap::new();
        for (idx, p) in f.params.iter().enumerate() {
            let TypeKind::Path(path) = &p.ty.kind else {
                continue;
            };
            if path.segments.last().map(String::as_str) != Some("Option") {
                continue;
            }
            let Some(args) = &path.generic_args else {
                continue;
            };
            if args.len() != 1 {
                continue;
            }
            let GenericArg::Type(inner) = &args[0] else {
                continue;
            };
            let TypeKind::Path(ip) = &inner.kind else {
                continue;
            };
            if ip.generic_args.as_ref().is_some_and(|a| !a.is_empty()) {
                continue;
            }
            let Some(member) = ip.segments.last() else {
                continue;
            };
            if self.cluster_link_struct(member).is_none() {
                continue;
            }
            let PatternKind::Binding(name) = &p.pattern.kind else {
                continue;
            };
            by_type
                .entry(member.clone())
                .or_default()
                .push((name.clone(), idx));
        }
        let mut out = Vec::new();
        for (member, params) in by_type {
            let Some((link_field, link_idx)) = self.cluster_link_struct(&member) else {
                continue;
            };
            let mut bindings = HashMap::new();
            for (name, _) in &params {
                bindings.insert(name.clone(), ClusterKind::OptionCursor);
            }
            let mut scan = ClusterScan {
                member_type: member.clone(),
                link_field,
                bindings,
                fresh: HashSet::new(),
                linked_once: HashSet::new(),
                root: Some(params[0].0.clone()),
                any_link: false,
                shadow_names: HashSet::new(),
                poisoned: None,
                free_member_literal: false,
                saw_boundary_region: false,
                annotation_mentions_member: false,
                adopted_root: None,
                borrowed: true,
                arg_borrow_positions: HashMap::new(),
                used_arg_sanction: false,
                pattern_members: HashSet::new(),
            };
            // Other params shadow as usual; the family's own roots are
            // exempt (they ARE params).
            let own: HashSet<&str> = params.iter().map(|(n, _)| n.as_str()).collect();
            for p in &f.params {
                for n in p.pattern.binding_names() {
                    if !own.contains(n.as_str()) {
                        scan.shadow_names.insert(n);
                    }
                }
            }
            self.cluster_collect_block(&f.body, &mut scan);
            if let Some(shadowed) = scan
                .bindings
                .keys()
                .find(|n| scan.shadow_names.contains(n.as_str()))
                .cloned()
            {
                scan.poison(
                    &format!("cluster name '{shadowed}' shadowed by a pattern binding"),
                    &f.span,
                );
            }
            for stmt in &f.body.stmts {
                self.cluster_verify_stmt(stmt, ClusterCtx::default(), &mut scan);
            }
            if let Some(e) = &f.body.final_expr {
                self.cluster_verify_expr(e, ClusterCtx::default(), &mut scan);
            }
            if scan.poisoned.is_none() {
                let mut bare_cursors = HashSet::new();
                let mut option_cursors = HashSet::new();
                for (n, kind) in &scan.bindings {
                    if own.contains(n.as_str()) {
                        continue; // params keep full registration
                    }
                    match kind {
                        ClusterKind::Bare => {
                            bare_cursors.insert(n.clone());
                        }
                        ClusterKind::OptionCursor => {
                            option_cursors.insert(n.clone());
                        }
                    }
                }
                out.push(ElidedCluster {
                    root: params[0].0.clone(),
                    member_type: member,
                    link_field_index: link_idx,
                    bindings: scan.bindings.keys().cloned().collect(),
                    b2: true,
                    fresh_linked: HashSet::new(),
                    bare_cursors,
                    option_cursors,
                    headerless: false,
                    returned: ReturnedChain::No,
                    adopted: false,
                    borrowed: true,
                    borrowed_params: params.clone(),
                    fn_pure: !scan.saw_boundary_region,
                    arg_sanctioned: false,
                });
            }
        }
        out
    }

    // ── Pass 1: membership ──────────────────────────────────────

    fn cluster_collect_block(&self, block: &Block, scan: &mut ClusterScan) {
        for stmt in &block.stmts {
            self.cluster_collect_stmt(stmt, scan);
        }
        if let Some(e) = &block.final_expr {
            self.cluster_collect_expr(e, scan);
        }
    }

    fn cluster_collect_stmt(&self, stmt: &Stmt, scan: &mut ClusterScan) {
        if let StmtKind::Let { pattern, value, .. } = &stmt.kind {
            if let PatternKind::Binding(name) = &pattern.kind {
                // Phase C1c: the adopted family's root is the candidate
                // binding itself — its sanctioned RHS is exactly the
                // builder call, not a member literal. Any OTHER let of
                // the candidate name (rebind) or of the same builder
                // call under a different name stays foreign (each
                // candidate gets its own independent scan).
                if let Some((root_name, builder)) = scan.adopted_root.clone() {
                    if name == &root_name && scan.root.is_none() {
                        let is_builder_call = matches!(&value.kind,
                            ExprKind::Call { callee, .. }
                                if matches!(&callee.kind,
                                    ExprKind::Identifier(c) if c == &builder));
                        if is_builder_call {
                            scan.bindings
                                .insert(name.clone(), ClusterKind::OptionCursor);
                            scan.root = Some(name.clone());
                        } else {
                            // First binding of the candidate name is
                            // not the builder call — the name-keyed
                            // family can't anchor.
                            scan.poison("adopted root bound to a non-builder RHS", &pattern.span);
                        }
                        self.cluster_collect_expr(value, scan);
                        return;
                    }
                }
                if scan.bindings.contains_key(name.as_str()) {
                    // Name rebound — name-keyed analysis can't track it.
                    scan.poison("cluster name bound more than once", &pattern.span);
                } else if let Some(kind) = self.classify_cluster_rhs(value, scan) {
                    scan.bindings.insert(name.clone(), kind);
                    if scan.root.is_none() {
                        // First cluster binding = root; must be a bare
                        // literal (link: None). Anything else cannot
                        // anchor the free-walk.
                        if is_member_literal_link_none(value, &scan.member_type, &scan.link_field) {
                            scan.root = Some(name.clone());
                        } else {
                            scan.poison(
                                "first cluster binding is not a literal root",
                                &pattern.span,
                            );
                        }
                    } else if is_member_literal_link_none(
                        value,
                        &scan.member_type,
                        &scan.link_field,
                    ) {
                        scan.fresh.insert(name.clone());
                    }
                }
            }
        }
        // Non-let pattern bindings shadow-collect.
        if let StmtKind::LetElse { pattern, .. } = &stmt.kind {
            for n in pattern.binding_names() {
                scan.shadow_names.insert(n);
            }
        }
        if let StmtKind::Let { pattern, .. } = &stmt.kind {
            if !matches!(&pattern.kind, PatternKind::Binding(_)) {
                for n in pattern.binding_names() {
                    scan.shadow_names.insert(n);
                }
            }
        }
        // Recurse for nested lets (loop bodies, branches).
        match &stmt.kind {
            StmtKind::MultiAssign { .. } => unreachable!(
                "StmtKind::MultiAssign is removed by the desugar pass before reaching this phase"
            ),
            StmtKind::Let { value, .. } | StmtKind::LetElse { value, .. } => {
                self.cluster_collect_expr(value, scan)
            }
            StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
                self.cluster_collect_expr(target, scan);
                self.cluster_collect_expr(value, scan);
            }
            StmtKind::Expr(e) => self.cluster_collect_expr(e, scan),
            StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                self.cluster_collect_block(body, scan)
            }
            StmtKind::LetUninit { .. } => {}
        }
    }

    fn cluster_collect_expr(&self, expr: &Expr, scan: &mut ClusterScan) {
        match &expr.kind {
            ExprKind::Block(b)
            | ExprKind::Seq(b)
            | ExprKind::Unsafe(b)
            | ExprKind::Try(b)
            | ExprKind::Par(b)
            | ExprKind::Lock { body: b, .. }
            | ExprKind::Loop { body: b, .. }
            | ExprKind::While { body: b, .. }
            | ExprKind::LabeledBlock { body: b, .. } => self.cluster_collect_block(b, scan),
            ExprKind::WhileLet {
                pattern,
                value,
                body: b,
                ..
            } => {
                // Same sanction as IfLet — `while let Some(n) = cur`.
                if let Some(n) = sanctioned_family_optional_bind(value, pattern, scan) {
                    scan.insert_pattern_member(n, &pattern.span);
                } else {
                    for n in pattern.binding_names() {
                        scan.shadow_names.insert(n);
                    }
                }
                self.cluster_collect_block(b, scan)
            }
            ExprKind::For {
                pattern, body: b, ..
            } => {
                for n in pattern.binding_names() {
                    scan.shadow_names.insert(n);
                }
                self.cluster_collect_block(b, scan)
            }
            ExprKind::If {
                then_block,
                else_branch,
                ..
            } => {
                self.cluster_collect_block(then_block, scan);
                if let Some(e) = else_branch {
                    self.cluster_collect_expr(e, scan);
                }
            }
            ExprKind::IfLet {
                pattern,
                value,
                then_block,
                else_branch,
            } => {
                // Phase C2a: the sanctioned optional bind on a family
                // option member (`if let Some(n) = a`) promotes `n` to
                // a Bare member instead of a shadow name — kata #2's
                // param-walk shape. Non-family if-lets keep today's
                // shadow behavior.
                if let Some(n) = sanctioned_family_optional_bind(value, pattern, scan) {
                    scan.insert_pattern_member(n, &pattern.span);
                } else {
                    for n in pattern.binding_names() {
                        scan.shadow_names.insert(n);
                    }
                }
                self.cluster_collect_block(then_block, scan);
                if let Some(e) = else_branch {
                    self.cluster_collect_expr(e, scan);
                }
            }
            ExprKind::Match { scrutinee, arms } => {
                // Phase C1c: the sanctioned read-only match on an
                // adopted-family option binding promotes its Some-arm
                // binding to a Bare member (verified by pass 2's
                // default-deny like any cursor) instead of a shadow
                // name. Collection is declaration-ordered, so the
                // scrutinee's option kind is already known here.
                if let Some(n) = sanctioned_family_match(scrutinee, arms, scan) {
                    scan.insert_pattern_member(n, &expr.span);
                    for arm in arms {
                        self.cluster_collect_expr(&arm.body, scan);
                    }
                    return;
                }
                for arm in arms {
                    for n in arm.pattern.binding_names() {
                        scan.shadow_names.insert(n);
                    }
                    self.cluster_collect_expr(&arm.body, scan);
                }
            }
            ExprKind::Closure { params, body, .. } => {
                for p in params {
                    for n in p.pattern.binding_names() {
                        scan.shadow_names.insert(n);
                    }
                }
                self.cluster_collect_expr(body, scan)
            }
            _ => {}
        }
    }

    /// Does this RHS make the binding a cluster member, and of which
    /// kind? `None` = unrelated binding (not part of the cluster).
    fn classify_cluster_rhs(&self, value: &Expr, scan: &ClusterScan) -> Option<ClusterKind> {
        match &value.kind {
            // Fresh member literal (root or fresh node) — only the
            // link:None form joins; a literal with any other link init
            // (e.g. the prepend idiom's `link: head`) is NOT a member,
            // and any cluster name inside it blocks in pass 2.
            ExprKind::StructLiteral { path, .. }
                if path.last().map(String::as_str) == Some(scan.member_type.as_str()) =>
            {
                // Phase C2a: borrowed-param scans treat member literals
                // as FOREIGN — the fn's own literal cluster (e.g.
                // add_two_numbers' dummy/tail/node triple) coexists
                // independently; dragging it into the borrow family
                // would conflate owned build traffic with the
                // non-owning walk.
                if scan.borrowed {
                    return None;
                }
                if is_member_literal_link_none(value, &scan.member_type, &scan.link_field) {
                    Some(ClusterKind::Bare)
                } else {
                    None
                }
            }
            // Bare alias: `let tail = dummy;`
            ExprKind::Identifier(n) if scan.kind_of(n) == Some(ClusterKind::Bare) => {
                Some(ClusterKind::Bare)
            }
            // Phase C1c/C2a (families only): option-cursor alias —
            // `let cur = out;` re-aims a non-owning cursor at the
            // family root (or another option cursor). Sound because
            // the family is read-only: nothing is freed or displaced
            // before the owner's drop, so the count-free copy can
            // never dangle. Literal clusters keep today's rule
            // (option cursors are born from link reads only).
            ExprKind::Identifier(n)
                if scan.is_family() && scan.kind_of(n) == Some(ClusterKind::OptionCursor) =>
            {
                Some(ClusterKind::OptionCursor)
            }
            // Link read: `let oc = x.link;`
            ExprKind::FieldAccess { object, field } if field == &scan.link_field => {
                match &object.kind {
                    ExprKind::Identifier(n) if scan.kind_of(n) == Some(ClusterKind::Bare) => {
                        Some(ClusterKind::OptionCursor)
                    }
                    _ => None,
                }
            }
            // Unwrap: `let n = oc.unwrap();`
            ExprKind::MethodCall { object, method, .. } if method == "unwrap" => {
                match &object.kind {
                    ExprKind::Identifier(n)
                        if scan.kind_of(n) == Some(ClusterKind::OptionCursor) =>
                    {
                        Some(ClusterKind::Bare)
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }

    // ── Pass 2: verification ────────────────────────────────────

    fn cluster_verify_block(&self, block: &Block, ctx: ClusterCtx, scan: &mut ClusterScan) {
        for stmt in &block.stmts {
            self.cluster_verify_stmt(stmt, ctx, scan);
        }
        if let Some(e) = &block.final_expr {
            self.cluster_verify_expr(e, ctx, scan);
        }
    }

    fn cluster_verify_stmt(&self, stmt: &Stmt, ctx: ClusterCtx, scan: &mut ClusterScan) {
        match &stmt.kind {
            StmtKind::MultiAssign { .. } => unreachable!(
                "StmtKind::MultiAssign is removed by the desugar pass before reaching this phase"
            ),
            StmtKind::Let {
                pattern, value, ty, ..
            } => {
                if let Some(te) = ty {
                    if type_expr_mentions_deep(te, &scan.member_type) {
                        scan.annotation_mentions_member = true;
                    }
                }
                let is_cluster_let = matches!(&pattern.kind, PatternKind::Binding(n)
                    if scan.bindings.contains_key(n.as_str()));
                if is_cluster_let && self.classify_cluster_rhs(value, scan).is_some() {
                    // Allowed membership shape — verify only the
                    // literal's prim inits (link init is None by
                    // construction; prim inits scan generically).
                    if let ExprKind::StructLiteral { fields, .. } = &value.kind {
                        for f in fields {
                            self.cluster_verify_expr(&f.value, ctx, scan);
                        }
                    }
                    // Identifier/link-read/unwrap RHS: the mention is
                    // the allowed alias — nothing further to verify.
                    return;
                }
                self.cluster_verify_expr(value, ctx, scan);
            }
            StmtKind::LetElse {
                value,
                else_block,
                ty,
                ..
            } => {
                if let Some(te) = ty {
                    if type_expr_mentions_deep(te, &scan.member_type) {
                        scan.annotation_mentions_member = true;
                    }
                }
                self.cluster_verify_expr(value, ctx, scan);
                self.cluster_verify_block(else_block, ctx, scan);
            }
            StmtKind::LetUninit { ty, .. } => {
                if type_expr_mentions_deep(ty, &scan.member_type) {
                    scan.annotation_mentions_member = true;
                }
            }
            StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                scan.saw_boundary_region = true;
                self.cluster_verify_block(body, ClusterCtx { boundary: true }, scan);
            }
            StmtKind::Assign { target, value } => {
                self.cluster_verify_assign(target, value, ctx, scan);
            }
            StmtKind::CompoundAssign { target, value, .. } => {
                // Compound ops only make sense on primitive fields.
                match &target.kind {
                    ExprKind::FieldAccess { object, field }
                        if field != &scan.link_field
                            && matches!(&object.kind, ExprKind::Identifier(n)
                                if scan.kind_of(n) == Some(ClusterKind::Bare)) =>
                    {
                        if ctx.boundary {
                            scan.poison("cluster use inside boundary region", &target.span);
                        }
                    }
                    _ => self.cluster_verify_expr(target, ctx, scan),
                }
                self.cluster_verify_expr(value, ctx, scan);
            }
            StmtKind::Expr(e) => self.cluster_verify_expr(e, ctx, scan),
        }
    }

    fn cluster_verify_assign(
        &self,
        target: &Expr,
        value: &Expr,
        ctx: ClusterCtx,
        scan: &mut ClusterScan,
    ) {
        // `x.link = Some(fresh)` / `x.link = None` — the append store.
        if let ExprKind::FieldAccess { object, field } = &target.kind {
            if let ExprKind::Identifier(obj) = &object.kind {
                if scan.kind_of(obj) == Some(ClusterKind::Bare) {
                    if ctx.boundary {
                        scan.poison("cluster use inside boundary region", &target.span);
                        return;
                    }
                    if field == &scan.link_field {
                        // Phase C1c/C2a: families are READ-ONLY.
                        // `Some(v)` stores already poison (no fresh
                        // set), but `= None` would also be unsound
                        // here: it severs the chain count-free (the
                        // family's cursors skip release-old), leaking
                        // the displaced tail past the owner's drop.
                        if scan.is_family() {
                            scan.poison("link store into a read-only family chain", &value.span);
                            return;
                        }
                        match link_value_shape(value) {
                            LinkValue::None => {}
                            LinkValue::SomeIdent(v) => {
                                if !scan.fresh.contains(v) {
                                    scan.poison(
                                        "link store of a non-fresh value (re-parenting)",
                                        &value.span,
                                    );
                                } else if !scan.linked_once.insert(v.to_string()) {
                                    scan.poison(
                                        "fresh node linked at more than one site",
                                        &value.span,
                                    );
                                } else {
                                    scan.any_link = true;
                                }
                            }
                            LinkValue::Other => {
                                scan.poison("unsupported link store value", &value.span);
                                self.cluster_verify_expr(value, ctx, scan);
                            }
                        }
                        return;
                    }
                    // Primitive field write — value scans generically.
                    self.cluster_verify_expr(value, ctx, scan);
                    return;
                }
            }
        }
        // Cursor reassignment: `c = <cluster expr>` for an existing
        // cluster binding of the matching kind.
        if let ExprKind::Identifier(t) = &target.kind {
            if let Some(kind) = scan.kind_of(t) {
                if ctx.boundary {
                    scan.poison("cluster use inside boundary region", &target.span);
                    return;
                }
                if scan.root.as_deref() == Some(t.as_str()) {
                    scan.poison("root reassigned", &target.span);
                    return;
                }
                match self.classify_cluster_rhs(value, scan) {
                    Some(k) if k == kind => {
                        // Allowed cursor advance. A literal RHS would
                        // re-bind a fresh node through an existing
                        // name — disallow (fresh nodes are let-born).
                        if matches!(&value.kind, ExprKind::StructLiteral { .. }) {
                            scan.poison("literal assigned to existing cursor", &value.span);
                        }
                        return;
                    }
                    _ => {
                        // `oc = None` resets an option cursor — allowed.
                        if kind == ClusterKind::OptionCursor
                            && matches!(&value.kind, ExprKind::Identifier(n) if n == "None")
                        {
                            return;
                        }
                        scan.poison("cluster binding assigned a non-cluster value", &value.span);
                        self.cluster_verify_expr(value, ctx, scan);
                        return;
                    }
                }
            }
        }
        self.cluster_verify_expr(target, ctx, scan);
        self.cluster_verify_expr(value, ctx, scan);
    }

    fn cluster_verify_expr(&self, expr: &Expr, ctx: ClusterCtx, scan: &mut ClusterScan) {
        match &expr.kind {
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

            // Default-deny: a bare cluster identifier in any context
            // not consumed by an allowed parent shape.
            ExprKind::Identifier(n) => {
                if scan.bindings.contains_key(n.as_str()) {
                    scan.poison(
                        "cluster binding escapes (alias/store/return/arg)",
                        &expr.span,
                    );
                }
            }
            ExprKind::Path { .. } => {}

            // Primitive field reads allowed anywhere; link reads only
            // via the let/assign shapes (consumed before descent), so
            // a link read reaching here blocks.
            ExprKind::FieldAccess { object, field } => {
                if let ExprKind::Identifier(n) = &object.kind {
                    if let Some(kind) = scan.kind_of(n) {
                        if ctx.boundary {
                            scan.poison("cluster use inside boundary region", &object.span);
                        } else if kind != ClusterKind::Bare || field == &scan.link_field {
                            scan.poison("link or option-cursor field escapes", &expr.span);
                        }
                        return;
                    }
                }
                self.cluster_verify_expr(object, ctx, scan);
            }

            // is_some/is_none on option cursors allowed; unwrap is only
            // allowed via the let/assign shapes (consumed earlier).
            ExprKind::MethodCall {
                object,
                method,
                args,
                ..
            } => {
                if let ExprKind::Identifier(n) = &object.kind {
                    if let Some(kind) = scan.kind_of(n) {
                        if ctx.boundary {
                            scan.poison("cluster use inside boundary region", &object.span);
                        } else if !(kind == ClusterKind::OptionCursor
                            && matches!(method.as_str(), "is_some" | "is_none"))
                        {
                            scan.poison("unsupported method on cluster binding", &expr.span);
                        }
                        for a in args {
                            self.cluster_verify_expr(&a.value, ctx, scan);
                        }
                        return;
                    }
                }
                self.cluster_verify_expr(object, ctx, scan);
                for a in args {
                    self.cluster_verify_expr(&a.value, ctx, scan);
                }
            }

            // Boundary regions: any cluster mention inside blocks.
            ExprKind::Closure { body, .. } => {
                scan.saw_boundary_region = true;
                self.cluster_verify_expr(body, ClusterCtx { boundary: true }, scan);
            }
            ExprKind::Par(b) | ExprKind::Lock { body: b, .. } => {
                scan.saw_boundary_region = true;
                self.cluster_verify_block(b, ClusterCtx { boundary: true }, scan);
            }

            // Generic recursion (same enumeration as phase A).
            ExprKind::InterpolatedStringLit(parts) => {
                for p in parts {
                    if let ParsedInterpolationPart::Expr(e) = p {
                        self.cluster_verify_expr(e, ctx, scan);
                    }
                }
            }
            ExprKind::Binary { left, right, .. } => {
                self.cluster_verify_expr(left, ctx, scan);
                self.cluster_verify_expr(right, ctx, scan);
            }
            ExprKind::Unary { operand, .. } => self.cluster_verify_expr(operand, ctx, scan),
            ExprKind::Question(e) => self.cluster_verify_expr(e, ctx, scan),
            ExprKind::OptionalChain { object, .. } => self.cluster_verify_expr(object, ctx, scan),
            ExprKind::NilCoalesce { left, right } => {
                self.cluster_verify_expr(left, ctx, scan);
                self.cluster_verify_expr(right, ctx, scan);
            }
            ExprKind::TupleIndex { object, .. } => self.cluster_verify_expr(object, ctx, scan),
            ExprKind::Index { object, index } => {
                self.cluster_verify_expr(object, ctx, scan);
                self.cluster_verify_expr(index, ctx, scan);
            }
            ExprKind::Call { callee, args } => {
                // Phase C2b sanctioned-arg channel (adopted scans
                // only): a family OPTION member passed at a borrowed
                // position of a summarized callee is an allowed use —
                // the callee walks it read-only and retains nothing.
                // Sound only under program-wide headerless-T (the
                // caller-side inc and callee exit dec are both
                // skipped); the post-filter and codegen's reconcile
                // deactivate the family if T fails the gate.
                let mut sanctioned_idx: HashSet<usize> = HashSet::new();
                if scan.adopted_root.is_some() && !ctx.boundary {
                    if let ExprKind::Identifier(g) = &callee.kind {
                        if let Some(positions) = scan.arg_borrow_positions.get(g.as_str()) {
                            for (i, a) in args.iter().enumerate() {
                                if positions.contains(&i)
                                    && matches!(&a.value.kind, ExprKind::Identifier(n)
                                        if scan.kind_of(n) == Some(ClusterKind::OptionCursor))
                                {
                                    sanctioned_idx.insert(i);
                                }
                            }
                        }
                    }
                }
                if !sanctioned_idx.is_empty() {
                    scan.used_arg_sanction = true;
                }
                self.cluster_verify_expr(callee, ctx, scan);
                for (i, a) in args.iter().enumerate() {
                    if !sanctioned_idx.contains(&i) {
                        self.cluster_verify_expr(&a.value, ctx, scan);
                    }
                }
            }
            ExprKind::Block(b) | ExprKind::Seq(b) | ExprKind::Unsafe(b) | ExprKind::Try(b) => {
                self.cluster_verify_block(b, ctx, scan);
            }
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                self.cluster_verify_expr(condition, ctx, scan);
                self.cluster_verify_block(then_block, ctx, scan);
                if let Some(e) = else_branch {
                    self.cluster_verify_expr(e, ctx, scan);
                }
            }
            ExprKind::IfLet {
                pattern,
                value,
                then_block,
                else_branch,
            } => {
                // Phase C2a: the sanctioned optional bind — the value
                // mention is the allowed read (the binding joined the
                // family in pass 1). Boundary regions keep poisoning.
                let sanctioned = !ctx.boundary
                    && sanctioned_family_optional_bind(value, pattern, scan).is_some();
                if !sanctioned {
                    self.cluster_verify_expr(value, ctx, scan);
                }
                self.cluster_verify_block(then_block, ctx, scan);
                if let Some(e) = else_branch {
                    self.cluster_verify_expr(e, ctx, scan);
                }
            }
            ExprKind::Match { scrutinee, arms } => {
                // Phase C1c: the sanctioned adopted-family match — the
                // scrutinee mention is the allowed read (the Some-arm
                // binding joined the family in pass 1; its uses verify
                // below like any cursor's). Guards are absent by
                // shape; boundary regions keep poisoning.
                if !ctx.boundary && sanctioned_family_match(scrutinee, arms, scan).is_some() {
                    for arm in arms {
                        self.cluster_verify_expr(&arm.body, ctx, scan);
                    }
                    return;
                }
                self.cluster_verify_expr(scrutinee, ctx, scan);
                for arm in arms {
                    if let Some(g) = &arm.guard {
                        self.cluster_verify_expr(g, ctx, scan);
                    }
                    self.cluster_verify_expr(&arm.body, ctx, scan);
                }
            }
            ExprKind::While {
                condition, body, ..
            } => {
                self.cluster_verify_expr(condition, ctx, scan);
                self.cluster_verify_block(body, ctx, scan);
            }
            ExprKind::WhileLet {
                pattern,
                value,
                body,
                ..
            } => {
                let sanctioned = !ctx.boundary
                    && sanctioned_family_optional_bind(value, pattern, scan).is_some();
                if !sanctioned {
                    self.cluster_verify_expr(value, ctx, scan);
                }
                self.cluster_verify_block(body, ctx, scan);
            }
            ExprKind::For { iterable, body, .. } => {
                self.cluster_verify_expr(iterable, ctx, scan);
                self.cluster_verify_block(body, ctx, scan);
            }
            ExprKind::Loop { body, .. } => self.cluster_verify_block(body, ctx, scan),
            ExprKind::LabeledBlock { body, .. } => self.cluster_verify_block(body, ctx, scan),
            ExprKind::Return(e) => {
                if let Some(e) = e {
                    self.cluster_verify_expr(e, ctx, scan);
                }
            }
            ExprKind::Break { value, .. } => {
                if let Some(e) = value {
                    self.cluster_verify_expr(e, ctx, scan);
                }
            }
            ExprKind::Tuple(es) | ExprKind::ArrayLiteral(es) => {
                for e in es {
                    self.cluster_verify_expr(e, ctx, scan);
                }
            }
            ExprKind::PrefixCollectionLiteral { items, .. } => {
                for e in items {
                    self.cluster_verify_expr(e, ctx, scan);
                }
            }
            ExprKind::RepeatLiteral { value, count, .. } => {
                self.cluster_verify_expr(value, ctx, scan);
                self.cluster_verify_expr(count, ctx, scan);
            }
            ExprKind::MapLiteral(pairs) => {
                for (k, v) in pairs {
                    self.cluster_verify_expr(k, ctx, scan);
                    self.cluster_verify_expr(v, ctx, scan);
                }
            }
            ExprKind::StructLiteral { path, fields, .. } => {
                // Phase D fn-purity: every sanctioned member-literal
                // position is consumed before descent (cluster-let RHS
                // returns early in `cluster_verify_stmt`; assign /
                // link-store literal shapes poison), so a member
                // literal reaching the generic walk is free-floating —
                // a headered-vs-headerless layout hazard. Harmless to
                // B1/B2 (it can never be linked), so demote D only.
                if path.last().map(String::as_str) == Some(scan.member_type.as_str()) {
                    scan.free_member_literal = true;
                }
                for f in fields {
                    self.cluster_verify_expr(&f.value, ctx, scan);
                }
            }
            ExprKind::Pipe { left, right } => {
                self.cluster_verify_expr(left, ctx, scan);
                self.cluster_verify_expr(right, ctx, scan);
            }
            ExprKind::Cast { expr: inner, .. } => self.cluster_verify_expr(inner, ctx, scan),
            ExprKind::Range { start, end, .. } => {
                if let Some(s) = start {
                    self.cluster_verify_expr(s, ctx, scan);
                }
                if let Some(e) = end {
                    self.cluster_verify_expr(e, ctx, scan);
                }
            }
            ExprKind::Providers { .. } => {
                scan.poison("unhandled construct (providers)", &expr.span);
            }
        }
    }
}

#[derive(Clone, Copy, Default)]
struct ClusterCtx {
    /// Inside a closure / par / lock / defer region — any cluster
    /// mention poisons.
    boundary: bool,
}

enum LinkValue<'e> {
    None,
    SomeIdent(&'e str),
    Other,
}

fn link_value_shape(value: &Expr) -> LinkValue<'_> {
    match &value.kind {
        ExprKind::Identifier(n) if n == "None" => LinkValue::None,
        ExprKind::Call { callee, args } if args.len() == 1 => match &callee.kind {
            ExprKind::Identifier(c) if c == "Some" => match &args[0].value.kind {
                ExprKind::Identifier(v) => LinkValue::SomeIdent(v),
                _ => LinkValue::Other,
            },
            _ => LinkValue::Other,
        },
        _ => LinkValue::Other,
    }
}

/// `T { ..., <link>: None }` — the fresh-member literal shape. A
/// missing link init is NOT accepted (the typechecker requires all
/// fields, so this is just defensive).
fn is_member_literal_link_none(value: &Expr, member: &str, link_field: &str) -> bool {
    let ExprKind::StructLiteral { path, fields, .. } = &value.kind else {
        return false;
    };
    if path.last().map(String::as_str) != Some(member) {
        return false;
    }
    fields.iter().any(|f| {
        f.name == link_field && matches!(&f.value.kind, ExprKind::Identifier(n) if n == "None")
    })
}

/// Phase C1c: recognize the sanctioned read-only match on an adopted
/// family's option binding:
///
/// ```text
/// match <option member> { Some(<binding>) => ..., None | _ => ... }
/// ```
///
/// Exactly two guard-free arms — one `Some(<plain binding>)`, one
/// `None` (or wildcard, equivalent for an Option scrutinee). Returns
/// the Some-arm binding name; it joins the family as a Bare member
/// (count-neutral in codegen: shared pattern bindings are borrowed
/// aliases — no inc, no per-arm cleanup). Adopted scans only: literal
/// clusters keep the default-deny on match scrutinees.
fn sanctioned_family_match(
    scrutinee: &Expr,
    arms: &[crate::ast::MatchArm],
    scan: &ClusterScan,
) -> Option<String> {
    if !scan.is_family() || arms.len() != 2 {
        return None;
    }
    match &scrutinee.kind {
        ExprKind::Identifier(s) if scan.kind_of(s) == Some(ClusterKind::OptionCursor) => {}
        _ => return None,
    }
    if arms.iter().any(|a| a.guard.is_some()) {
        return None;
    }
    let mut some_binding: Option<String> = None;
    let mut saw_none = false;
    for arm in arms {
        match &arm.pattern.kind {
            PatternKind::TupleVariant { path, patterns }
                if path.last().map(String::as_str) == Some("Some") && patterns.len() == 1 =>
            {
                if let PatternKind::Binding(n) = &patterns[0].kind {
                    if some_binding.is_some() {
                        return None;
                    }
                    some_binding = Some(n.clone());
                } else {
                    return None;
                }
            }
            PatternKind::Binding(n) if n == "None" => saw_none = true,
            PatternKind::Wildcard => saw_none = true,
            _ => return None,
        }
    }
    if !saw_none {
        return None;
    }
    some_binding
}

/// Phase C2b: exhaustive expression visitor — every `Expr` in a body,
/// with a flag for direct-call CALLEE position. Serves the two gate
/// scans that must not miss a position: the fn-as-value scan (a
/// summarized builder/borrower referenced as a value would create an
/// unsummarized indirect call site — the residual-count contract
/// breaks) and the builder-call-site count (every T-builder call must
/// sit in adopted-let position). Explicit exhaustive match, no
/// wildcard: a future ExprKind variant must be classified here before
/// it compiles.
fn walk_fn_exprs(block: &Block, f: &mut impl FnMut(&Expr, bool)) {
    fn walk(e: &Expr, is_callee: bool, f: &mut impl FnMut(&Expr, bool)) {
        f(e, is_callee);
        match &e.kind {
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
                    if let ParsedInterpolationPart::Expr(inner) = p {
                        walk(inner, false, f);
                    }
                }
            }
            ExprKind::Binary { left, right, .. } | ExprKind::NilCoalesce { left, right } => {
                walk(left, false, f);
                walk(right, false, f);
            }
            ExprKind::Pipe { left, right } => {
                // The pipe RHS is call-like but may also be a bare
                // fn reference — conservatively NOT callee position
                // (the fn-as-value scan must flag `x |> f`).
                walk(left, false, f);
                walk(right, false, f);
            }
            ExprKind::Unary { operand, .. } => walk(operand, false, f),
            ExprKind::Question(inner) | ExprKind::Cast { expr: inner, .. } => walk(inner, false, f),
            ExprKind::OptionalChain { object, args, .. } => {
                walk(object, false, f);
                if let Some(args) = args {
                    for a in args {
                        walk(&a.value, false, f);
                    }
                }
            }
            ExprKind::Call { callee, args } => {
                walk(callee, true, f);
                for a in args {
                    walk(&a.value, false, f);
                }
            }
            ExprKind::MethodCall { object, args, .. } => {
                walk(object, false, f);
                for a in args {
                    walk(&a.value, false, f);
                }
            }
            ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
                walk(object, false, f)
            }
            ExprKind::Index { object, index } => {
                walk(object, false, f);
                walk(index, false, f);
            }
            ExprKind::Block(b)
            | ExprKind::Seq(b)
            | ExprKind::Unsafe(b)
            | ExprKind::Try(b)
            | ExprKind::Par(b)
            | ExprKind::Loop { body: b, .. }
            | ExprKind::LabeledBlock { body: b, .. } => walk_fn_exprs(b, f),
            ExprKind::Lock { mutex, body, .. } => {
                walk(mutex, false, f);
                walk_fn_exprs(body, f);
            }
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                walk(condition, false, f);
                walk_fn_exprs(then_block, f);
                if let Some(e2) = else_branch {
                    walk(e2, false, f);
                }
            }
            ExprKind::IfLet {
                value,
                then_block,
                else_branch,
                ..
            } => {
                walk(value, false, f);
                walk_fn_exprs(then_block, f);
                if let Some(e2) = else_branch {
                    walk(e2, false, f);
                }
            }
            ExprKind::Match { scrutinee, arms } => {
                walk(scrutinee, false, f);
                for arm in arms {
                    if let Some(g) = &arm.guard {
                        walk(g, false, f);
                    }
                    walk(&arm.body, false, f);
                }
            }
            ExprKind::While {
                condition, body, ..
            } => {
                walk(condition, false, f);
                walk_fn_exprs(body, f);
            }
            ExprKind::WhileLet { value, body, .. } => {
                walk(value, false, f);
                walk_fn_exprs(body, f);
            }
            ExprKind::For { iterable, body, .. } => {
                walk(iterable, false, f);
                walk_fn_exprs(body, f);
            }
            ExprKind::Closure { body, .. } => walk(body, false, f),
            ExprKind::Return(inner) => {
                if let Some(inner) = inner {
                    walk(inner, false, f);
                }
            }
            ExprKind::Break { value, .. } => {
                if let Some(v) = value {
                    walk(v, false, f);
                }
            }
            ExprKind::Tuple(es)
            | ExprKind::ArrayLiteral(es)
            | ExprKind::PrefixCollectionLiteral { items: es, .. } => {
                for e2 in es {
                    walk(e2, false, f);
                }
            }
            ExprKind::RepeatLiteral { value, count, .. } => {
                walk(value, false, f);
                walk(count, false, f);
            }
            ExprKind::MapLiteral(pairs) => {
                for (k, v) in pairs {
                    walk(k, false, f);
                    walk(v, false, f);
                }
            }
            ExprKind::StructLiteral { fields, .. } => {
                for fi in fields {
                    walk(&fi.value, false, f);
                }
            }
            ExprKind::Range { start, end, .. } => {
                if let Some(st) = start {
                    walk(st, false, f);
                }
                if let Some(en) = end {
                    walk(en, false, f);
                }
            }
            ExprKind::Providers { body, .. } => walk_fn_exprs(body, f),
        }
    }
    for stmt in &block.stmts {
        match &stmt.kind {
            StmtKind::MultiAssign { .. } => unreachable!(
                "StmtKind::MultiAssign is removed by the desugar pass before reaching this phase"
            ),
            StmtKind::Let { value, .. } => walk(value, false, f),
            StmtKind::LetElse {
                value, else_block, ..
            } => {
                walk(value, false, f);
                walk_fn_exprs(else_block, f);
            }
            StmtKind::LetUninit { .. } => {}
            StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
                walk(target, false, f);
                walk(value, false, f);
            }
            StmtKind::Expr(e) => walk(e, false, f),
            StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => walk_fn_exprs(body, f),
        }
    }
    if let Some(e) = &block.final_expr {
        walk(e, false, f);
    }
}

/// Phase C2b: exact `Option[<member>]` shape test (no nesting, no
/// generic args on the member) — the only T-mentioning param/return
/// shape the headerless gate accepts on free fns.
fn is_exactly_option_of(te: &TypeExpr, member: &str) -> bool {
    let TypeKind::Path(path) = &te.kind else {
        return false;
    };
    if path.segments.last().map(String::as_str) != Some("Option") {
        return false;
    }
    let Some(args) = &path.generic_args else {
        return false;
    };
    if args.len() != 1 {
        return false;
    }
    let GenericArg::Type(inner) = &args[0] else {
        return false;
    };
    let TypeKind::Path(ip) = &inner.kind else {
        return false;
    };
    ip.segments.last().map(String::as_str) == Some(member)
        && ip.generic_args.as_ref().is_none_or(|a| a.is_empty())
}

/// Phase C2a: recognize the sanctioned optional bind on a family
/// option member — `if let Some(<binding>) = <option member>` /
/// `while let Some(<binding>) = <option member>`. The binding joins
/// the family as a Bare member (count-neutral in codegen, like the
/// match arm's). Family scans only.
fn sanctioned_family_optional_bind(
    value: &Expr,
    pattern: &Pattern,
    scan: &ClusterScan,
) -> Option<String> {
    if !scan.is_family() {
        return None;
    }
    match &value.kind {
        ExprKind::Identifier(s) if scan.kind_of(s) == Some(ClusterKind::OptionCursor) => {}
        _ => return None,
    }
    match &pattern.kind {
        PatternKind::TupleVariant { path, patterns }
            if path.last().map(String::as_str) == Some("Some") && patterns.len() == 1 =>
        {
            if let PatternKind::Binding(n) = &patterns[0].kind {
                Some(n.clone())
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Phase-D deep type mention scan: recurses through every
/// type-carrying `TypeKind` shape (tuples, arrays, pointers, fn types,
/// slices, weak refs, impl/dyn generic args). Unknown / future variants
/// answer `true` — a missed mention is a layout-corruption hazard, so
/// the helper fails toward "mentions" (demote headerless, keep B1/B2).
fn type_expr_mentions_deep(te: &TypeExpr, name: &str) -> bool {
    let args_mention = |args: &Vec<GenericArg>| {
        args.iter().any(|a| match a {
            GenericArg::Type(t) => type_expr_mentions_deep(t, name),
            _ => false,
        })
    };
    match &te.kind {
        TypeKind::Path(p) => {
            p.segments.last().map(String::as_str) == Some(name)
                || p.generic_args.as_ref().is_some_and(&args_mention)
        }
        TypeKind::Tuple(ts) => ts.iter().any(|t| type_expr_mentions_deep(t, name)),
        TypeKind::Array { element, .. } => type_expr_mentions_deep(element, name),
        TypeKind::Pointer { inner, .. }
        | TypeKind::Ref(inner)
        | TypeKind::MutRef(inner)
        | TypeKind::MutSlice(inner)
        | TypeKind::Weak(inner) => type_expr_mentions_deep(inner, name),
        TypeKind::FnType {
            params,
            return_type,
            ..
        } => {
            params.iter().any(|t| type_expr_mentions_deep(t, name))
                || return_type
                    .as_ref()
                    .is_some_and(|t| type_expr_mentions_deep(t, name))
        }
        TypeKind::ImplTrait { args, .. } | TypeKind::Dyn { args, .. } => args_mention(args),
        TypeKind::Unit | TypeKind::Error => false,
    }
}

impl<'a> OwnershipChecker<'a> {
    /// Phase-D program purity: true when any declared type, signature,
    /// alias, or module-level binding in the program mentions `member`
    /// — i.e. a headered `member` value could cross a function boundary
    /// (in either direction) through a declared surface, which would
    /// break per-`(fn, type)` headerless layout keying. Function and
    /// test BODIES are deliberately not scanned: a `member` value
    /// constructed inside another fn stays headered AND stays inside
    /// that fn unless some declared surface (scanned here) lets it out.
    fn program_leaks_member_type(&self, member: &str) -> bool {
        self.program_member_scan(member, false)
    }

    /// Phase D / C2b shared program-surface scan. `lenient_free_fns ==
    /// false` is phase D's rule: ANY signature mention blocks.
    /// `true` is C2b's: a FREE fn may mention the member as exactly
    /// `Option[member]` in param/return position (those are the
    /// borrowed-param / fresh-return channels the per-fn coverage
    /// check then validates); any other free-fn shape — and any
    /// method/trait/extern mention in either mode — still blocks.
    fn program_member_scan(&self, member: &str, lenient_free_fns: bool) -> bool {
        let m = member;
        let param_or_ret = |params: &Vec<Param>, ret: &Option<TypeExpr>| {
            params.iter().any(|p| type_expr_mentions_deep(&p.ty, m))
                || ret.as_ref().is_some_and(|t| type_expr_mentions_deep(t, m))
        };
        let lenient_param_or_ret = |params: &Vec<Param>, ret: &Option<TypeExpr>| {
            params
                .iter()
                .any(|p| type_expr_mentions_deep(&p.ty, m) && !is_exactly_option_of(&p.ty, m))
                || ret
                    .as_ref()
                    .is_some_and(|t| type_expr_mentions_deep(t, m) && !is_exactly_option_of(t, m))
        };
        for item in &self.program.items {
            let leaks = match item {
                Item::Function(g) => {
                    if lenient_free_fns {
                        lenient_param_or_ret(&g.params, &g.return_type)
                    } else {
                        param_or_ret(&g.params, &g.return_type)
                    }
                }
                Item::StructDef(s) => {
                    s.name != m && s.fields.iter().any(|f| type_expr_mentions_deep(&f.ty, m))
                }
                Item::UnionDef(u) => u.fields.iter().any(|f| type_expr_mentions_deep(&f.ty, m)),
                Item::EnumDef(e) => e.variants.iter().any(|v| match &v.kind {
                    VariantKind::Unit => false,
                    VariantKind::Tuple(ts) => ts.iter().any(|t| type_expr_mentions_deep(t, m)),
                    VariantKind::Struct(fs) => fs.iter().any(|f| type_expr_mentions_deep(&f.ty, m)),
                }),
                Item::TraitDef(t) => t.items.iter().any(|ti| match ti {
                    TraitItem::Method(tm) => param_or_ret(&tm.params, &tm.return_type),
                    // Assoc-type declarations carry bounds, not
                    // concrete types; the concrete leak surface is the
                    // impl-side binding (scanned below).
                    TraitItem::AssocType(_) => false,
                }),
                Item::ImplBlock(imp) => {
                    // Coarse v1: ANY impl whose target mentions the
                    // member type demotes (its methods receive
                    // headered `self`); plus method sigs and GAT
                    // bindings on impls of other types.
                    type_expr_mentions_deep(&imp.target_type, m)
                        || imp.items.iter().any(|ii| match ii {
                            ImplItem::Method(f) => param_or_ret(&f.params, &f.return_type),
                            ImplItem::AssocType(b) => type_expr_mentions_deep(&b.ty, m),
                        })
                }
                Item::ExternFunction(ef) => param_or_ret(&ef.params, &ef.return_type),
                Item::ExternBlock(eb) => eb.items.iter().any(|ei| match ei {
                    ExternItem::Function(ef) => param_or_ret(&ef.params, &ef.return_type),
                    _ => false,
                }),
                Item::ConstDecl(c) => type_expr_mentions_deep(&c.ty, m),
                Item::ModuleBinding(mb) => mb
                    .ty
                    .as_ref()
                    .is_some_and(|t| type_expr_mentions_deep(t, m)),
                // Aliases can smuggle the member under another name
                // past every other check — any alias that RESOLVES to
                // mention the member demotes outright.
                Item::TypeAlias(ta) => type_expr_mentions_deep(&ta.ty, m),
                Item::DistinctType(dt) => type_expr_mentions_deep(&dt.base_type, m),
                // A layout block re-describes a collection's physical
                // form; one naming the member type implies a foreign
                // layout authority over it.
                Item::LayoutDef(ld) => type_expr_mentions_deep(&ld.collection_type, m),
                // No type-carrying surface that can move a value.
                Item::TraitAlias(_)
                | Item::MarkerTrait(_)
                | Item::EffectResource(_)
                | Item::EffectGroup(_)
                | Item::EffectVerbDecl(_)
                | Item::UseDecl(_)
                | Item::Import(_)
                | Item::AliasDecl(_)
                | Item::IndependentDecl(_)
                | Item::TestCase(_) => false,
            };
            if leaks {
                return true;
            }
        }
        false
    }

    /// Phase C2b coverage: does `f` confine every value of `t` it
    /// touches to count-free families? Returns `(touches, covered)`.
    /// `allow_params` is false for methods (borrowed families are
    /// free-fn-only in v1 — but a method T-param already failed the
    /// signature scan, so this is belt-and-suspenders).
    #[allow(clippy::too_many_arguments)]
    fn fn_covers_member(
        &self,
        f: &Function,
        fn_key: &str,
        t: &str,
        clusters: &HashMap<String, Vec<ElidedCluster>>,
        builders: &HashMap<String, (String, usize)>,
        allow_params: bool,
    ) -> (bool, bool) {
        let fn_clusters: &[ElidedCluster] = clusters.get(fn_key).map(Vec::as_slice).unwrap_or(&[]);
        let of_t: Vec<&ElidedCluster> = fn_clusters.iter().filter(|c| c.member_type == t).collect();
        let mut lits_present = false;
        collect_struct_literal_types(&f.body, &mut |n| {
            if n == t {
                lits_present = true;
            }
        });
        let t_params: Vec<usize> = f
            .params
            .iter()
            .enumerate()
            .filter(|(_, p)| type_expr_mentions_deep(&p.ty, t))
            .map(|(i, _)| i)
            .collect();
        let ret_t = f
            .return_type
            .as_ref()
            .is_some_and(|r| type_expr_mentions_deep(r, t));
        let mut builder_sites = 0usize;
        walk_fn_exprs(&f.body, &mut |e, _| {
            if let ExprKind::Call { callee, .. } = &e.kind {
                if let ExprKind::Identifier(b) = &callee.kind {
                    if builders.get(b.as_str()).is_some_and(|(m, _)| m == t) {
                        builder_sites += 1;
                    }
                }
            }
        });
        let touches =
            lits_present || !t_params.is_empty() || ret_t || builder_sites > 0 || !of_t.is_empty();
        if !touches {
            return (false, true);
        }
        // Literal rule: every T literal sits in a b2 literal cluster
        // whose fn-purity flags are clean (no free literals, no
        // boundary regions).
        let lit_cluster = of_t.iter().find(|c| !c.adopted && !c.borrowed);
        if lits_present {
            match lit_cluster {
                Some(c) if c.b2 && c.fn_pure => {}
                _ => return (true, false),
            }
        }
        // Return rule: a T-returning fn must be a fresh-return builder.
        if ret_t {
            match lit_cluster {
                Some(c) if c.returned != ReturnedChain::No => {}
                _ => return (true, false),
            }
        }
        // Param rule: every T-param is covered by the borrowed family.
        if !t_params.is_empty() {
            if !allow_params {
                return (true, false);
            }
            let Some(bf) = of_t.iter().find(|c| c.borrowed) else {
                return (true, false);
            };
            if !bf.fn_pure {
                return (true, false);
            }
            let covered: HashSet<usize> = bf.borrowed_params.iter().map(|(_, i)| *i).collect();
            if t_params.iter().any(|i| !covered.contains(i)) {
                return (true, false);
            }
        }
        // Builder-call rule: every T-builder call site is an
        // adopted-root let (count match — the candidate walker only
        // records let-position sites, and each adoption consumed one).
        let adopted: Vec<&&ElidedCluster> = of_t.iter().filter(|c| c.adopted).collect();
        if builder_sites != adopted.len() {
            return (true, false);
        }
        if adopted.iter().any(|c| !c.fn_pure) {
            return (true, false);
        }
        (true, true)
    }

    /// Phase C2b: the program-wide headerless-T gate (analysis half;
    /// codegen reconciles against coroutine compilation and link-niche
    /// shape). Returns `T → (link index, touching fn keys)`.
    fn compute_headerless_types(
        &self,
        clusters: &HashMap<String, Vec<ElidedCluster>>,
        builders: &HashMap<String, (String, usize)>,
        borrows: &HashMap<String, (String, HashSet<usize>)>,
    ) -> HashMap<String, (usize, Vec<String>)> {
        let mut out = HashMap::new();
        let types: HashSet<String> = clusters
            .values()
            .flatten()
            .map(|c| c.member_type.clone())
            .collect();
        'types: for t in types {
            let Some((_, link_idx)) = self.cluster_link_struct(&t) else {
                continue;
            };
            // Surface scan with the free-fn Option[T] leniency.
            if self.program_member_scan(&t, true) {
                continue;
            }
            // The summarized fns whose two-sided contracts the gate
            // activates — a reference to any of them as a VALUE would
            // create an unsummarized indirect call site.
            let protected: HashSet<&str> = builders
                .iter()
                .filter(|(_, (m, _))| m == &t)
                .map(|(k, _)| k.as_str())
                .chain(
                    borrows
                        .iter()
                        .filter(|(_, (m, _))| m == &t)
                        .map(|(k, _)| k.as_str()),
                )
                .collect();
            let mut touching: Vec<String> = Vec::new();
            for item in &self.program.items {
                match item {
                    Item::Function(f) => {
                        let mut fn_as_value = false;
                        walk_fn_exprs(&f.body, &mut |e, is_callee| {
                            if let ExprKind::Identifier(n) = &e.kind {
                                if !is_callee && protected.contains(n.as_str()) {
                                    fn_as_value = true;
                                }
                            }
                        });
                        if fn_as_value {
                            continue 'types;
                        }
                        let (touches, covered) =
                            self.fn_covers_member(f, &f.name, &t, clusters, builders, true);
                        if touches {
                            if !covered {
                                continue 'types;
                            }
                            touching.push(f.name.clone());
                        }
                    }
                    Item::ImplBlock(imp) => {
                        let type_name = match &imp.target_type.kind {
                            TypeKind::Path(p) => p.segments.last().cloned().unwrap_or_default(),
                            _ => continue,
                        };
                        for ii in &imp.items {
                            if let ImplItem::Method(method) = ii {
                                let key = format!("{}.{}", type_name, method.name);
                                let mut fn_as_value = false;
                                walk_fn_exprs(&method.body, &mut |e, is_callee| {
                                    if let ExprKind::Identifier(n) = &e.kind {
                                        if !is_callee && protected.contains(n.as_str()) {
                                            fn_as_value = true;
                                        }
                                    }
                                });
                                if fn_as_value {
                                    continue 'types;
                                }
                                let (touches, covered) = self
                                    .fn_covers_member(method, &key, &t, clusters, builders, false);
                                if touches {
                                    if !covered {
                                        continue 'types;
                                    }
                                    touching.push(key);
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            out.insert(t, (link_idx, touching));
        }
        out
    }
}

/// Pre-pass: every struct-literal type name in the body.
/// Phase C1c: collect adoption candidates — `let <name> =
/// <builder>(...)` statements whose callee is a bare Identifier with a
/// fresh-return summary. Descends nested blocks / loops / branches /
/// match arms but NOT closure / par / lock / defer bodies: those
/// compile under a different fn key, so an adopted root inside one
/// would never get its let-site cleanup queued (the family would
/// silently leak instead of falling back to full RC).
fn collect_adoption_candidates(
    block: &Block,
    summaries: &HashMap<String, (String, usize)>,
    out: &mut Vec<(String, String)>,
) {
    fn walk_expr(
        e: &Expr,
        summaries: &HashMap<String, (String, usize)>,
        out: &mut Vec<(String, String)>,
    ) {
        match &e.kind {
            ExprKind::Block(b)
            | ExprKind::Seq(b)
            | ExprKind::Unsafe(b)
            | ExprKind::Try(b)
            | ExprKind::Loop { body: b, .. }
            | ExprKind::While { body: b, .. }
            | ExprKind::WhileLet { body: b, .. }
            | ExprKind::For { body: b, .. }
            | ExprKind::LabeledBlock { body: b, .. } => {
                collect_adoption_candidates(b, summaries, out)
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
                collect_adoption_candidates(then_block, summaries, out);
                if let Some(e2) = else_branch {
                    walk_expr(e2, summaries, out);
                }
            }
            ExprKind::Match { arms, .. } => {
                for arm in arms {
                    walk_expr(&arm.body, summaries, out);
                }
            }
            // Closure / Par / Lock / Defer: deliberately not descended.
            _ => {}
        }
    }
    for stmt in &block.stmts {
        match &stmt.kind {
            StmtKind::MultiAssign { .. } => unreachable!(
                "StmtKind::MultiAssign is removed by the desugar pass before reaching this phase"
            ),
            StmtKind::Let { pattern, value, .. } => {
                if let (PatternKind::Binding(name), ExprKind::Call { callee, .. }) =
                    (&pattern.kind, &value.kind)
                {
                    if let ExprKind::Identifier(b) = &callee.kind {
                        if summaries.contains_key(b.as_str()) {
                            out.push((name.clone(), b.clone()));
                        }
                    }
                }
                walk_expr(value, summaries, out);
            }
            StmtKind::LetElse { value, .. } => walk_expr(value, summaries, out),
            StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
                walk_expr(target, summaries, out);
                walk_expr(value, summaries, out);
            }
            StmtKind::Expr(e) => walk_expr(e, summaries, out),
            StmtKind::Defer { .. } | StmtKind::ErrDefer { .. } | StmtKind::LetUninit { .. } => {}
        }
    }
    if let Some(e) = &block.final_expr {
        walk_expr(e, summaries, out);
    }
}

fn collect_struct_literal_types(block: &Block, f: &mut impl FnMut(&str)) {
    fn walk_expr(e: &Expr, f: &mut impl FnMut(&str)) {
        if let ExprKind::StructLiteral { path, fields, .. } = &e.kind {
            if let Some(n) = path.last() {
                f(n);
            }
            for fi in fields {
                walk_expr(&fi.value, f);
            }
            return;
        }
        // Containers that can host literals.
        match &e.kind {
            ExprKind::Block(b)
            | ExprKind::Seq(b)
            | ExprKind::Unsafe(b)
            | ExprKind::Try(b)
            | ExprKind::Par(b)
            | ExprKind::Lock { body: b, .. }
            | ExprKind::Loop { body: b, .. }
            | ExprKind::While { body: b, .. }
            | ExprKind::WhileLet { body: b, .. }
            | ExprKind::For { body: b, .. }
            | ExprKind::LabeledBlock { body: b, .. } => collect_struct_literal_types(b, f),
            ExprKind::If {
                then_block,
                else_branch,
                ..
            } => {
                collect_struct_literal_types(then_block, f);
                if let Some(e2) = else_branch {
                    walk_expr(e2, f);
                }
            }
            ExprKind::IfLet {
                then_block,
                else_branch,
                ..
            } => {
                collect_struct_literal_types(then_block, f);
                if let Some(e2) = else_branch {
                    walk_expr(e2, f);
                }
            }
            ExprKind::Match { arms, .. } => {
                for arm in arms {
                    walk_expr(&arm.body, f);
                }
            }
            ExprKind::Closure { body, .. } => walk_expr(body, f),
            _ => {}
        }
    }
    for stmt in &block.stmts {
        match &stmt.kind {
            StmtKind::MultiAssign { .. } => unreachable!(
                "StmtKind::MultiAssign is removed by the desugar pass before reaching this phase"
            ),
            StmtKind::Let { value, .. } | StmtKind::LetElse { value, .. } => walk_expr(value, f),
            StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
                walk_expr(target, f);
                walk_expr(value, f);
            }
            StmtKind::Expr(e) => walk_expr(e, f),
            StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                collect_struct_literal_types(body, f)
            }
            StmtKind::LetUninit { .. } => {}
        }
    }
}

// ── Phase B2 recognizer ─────────────────────────────────────────
//
// Approves build-side count-op elision for a B1-verified cluster when
// displacement is structurally impossible and no cursor can observe a
// freed node:
//
//   1. exactly ONE link-store site in the function;
//   2. that site is either (a) outside every loop (it executes at most
//      once, the target field starts None — fresh literals carry
//      `link: None` — so nothing is ever displaced), or (b) the
//      canonical adjacent append TRIPLE inside a loop body:
//          let <node> = T { ..., link: None };
//          <cursor>.link = Some(<node>);
//          <cursor> = <node>;
//      — the advance immediately after the store means each dynamic
//      target instance is a freshly appended node whose link is still
//      None, so the store never displaces;
//   3. every link READ (option-cursor creation) occurs strictly after
//      the store region (the loop's exit for (b), the store statement
//      for (a)) in pre-order statement order — so no alias into the
//      chain exists while it is still being built;
//   4. every fresh literal binding is consumed by the link store
//      (`fresh_unlinked` empty) — never-linked fresh nodes would need
//      their own mid-scope frees, which a live outer cursor could
//      observe.
//
// Under 1–4 nothing is freed before the root's scope-exit free-walk,
// so count-free cursors can never dangle, and the elided link store
// reduces to a single pointer store.

/// Returns `Some(fresh_linked)` when the cluster qualifies for B2.
fn recognize_b2(f: &Function, scan: &ClusterScan) -> Option<HashSet<String>> {
    let mut rec = B2Rec {
        scan,
        counter: 0,
        stores: Vec::new(),
        reads: Vec::new(),
        loop_depth: 0,
    };
    rec.walk_block(&f.body);
    // Rule 1: exactly one link-store site.
    if rec.stores.len() != 1 {
        return None;
    }
    let store = &rec.stores[0];
    // Rule 2: outside loops, or the canonical triple.
    let region_end = if store.loop_depth == 0 {
        store.counter
    } else if store.is_triple {
        store.loop_exit_counter
    } else {
        return None;
    };
    // Rule 3: reads strictly after the store region.
    if rec.reads.iter().any(|&r| r <= region_end) {
        return None;
    }
    // Rule 4: every fresh binding is the linked one.
    let linked: HashSet<String> = [store.value_name.clone()].into_iter().collect();
    if scan.fresh.iter().any(|n| !linked.contains(n)) {
        return None;
    }
    Some(linked)
}

struct StoreSite {
    counter: usize,
    loop_depth: usize,
    is_triple: bool,
    /// Pre-order counter at the enclosing loop's exit (only meaningful
    /// when `is_triple`).
    loop_exit_counter: usize,
    /// The fresh binding consumed (`Some(<name>)`).
    value_name: String,
}

struct B2Rec<'s> {
    scan: &'s ClusterScan,
    counter: usize,
    stores: Vec<StoreSite>,
    reads: Vec<usize>,
    loop_depth: usize,
}

impl B2Rec<'_> {
    fn walk_block(&mut self, block: &Block) {
        let stmts = &block.stmts;
        let mut i = 0;
        while i < stmts.len() {
            self.counter += 1;
            // Triple detection at this position: [let node = lit;
            // cursor.link = Some(node); cursor = node;]
            if self.loop_depth > 0 && i + 2 < stmts.len() {
                if let Some(node) = self.triple_at(&stmts[i], &stmts[i + 1], &stmts[i + 2]) {
                    let store_counter = self.counter + 1;
                    self.stores.push(StoreSite {
                        counter: store_counter,
                        loop_depth: self.loop_depth,
                        is_triple: true,
                        loop_exit_counter: 0, // patched at loop exit
                        value_name: node,
                    });
                    self.counter += 2; // the store + advance stmts
                    i += 3;
                    continue;
                }
            }
            self.walk_stmt(&stmts[i]);
            i += 1;
        }
        if let Some(e) = &block.final_expr {
            self.counter += 1;
            self.walk_expr(e);
        }
    }

    fn triple_at(&self, s0: &Stmt, s1: &Stmt, s2: &Stmt) -> Option<String> {
        // s0: let <node> = T { ..., link: None };  (a fresh binding)
        let node = match &s0.kind {
            StmtKind::Let { pattern, value, .. } => match &pattern.kind {
                PatternKind::Binding(n)
                    if self.scan.fresh.contains(n.as_str())
                        && is_member_literal_link_none(
                            value,
                            &self.scan.member_type,
                            &self.scan.link_field,
                        ) =>
                {
                    n.clone()
                }
                _ => return None,
            },
            _ => return None,
        };
        // s1: <cursor>.link = Some(<node>);
        match &s1.kind {
            StmtKind::Assign { target, value } => {
                let ExprKind::FieldAccess { object, field } = &target.kind else {
                    return None;
                };
                if field != &self.scan.link_field {
                    return None;
                }
                let ExprKind::Identifier(cursor) = &object.kind else {
                    return None;
                };
                if self.scan.kind_of(cursor) != Some(ClusterKind::Bare) {
                    return None;
                }
                match link_value_shape(value) {
                    LinkValue::SomeIdent(v) if v == node => {}
                    _ => return None,
                }
                // s2: <cursor> = <node>;
                match &s2.kind {
                    StmtKind::Assign {
                        target: t2,
                        value: v2,
                    } => {
                        let (ExprKind::Identifier(t), ExprKind::Identifier(v)) =
                            (&t2.kind, &v2.kind)
                        else {
                            return None;
                        };
                        if t == cursor && v == &node {
                            Some(node)
                        } else {
                            None
                        }
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }

    fn walk_stmt(&mut self, stmt: &Stmt) {
        match &stmt.kind {
            StmtKind::MultiAssign { .. } => unreachable!(
                "StmtKind::MultiAssign is removed by the desugar pass before reaching this phase"
            ),
            StmtKind::Let { value, .. } | StmtKind::LetElse { value, .. } => {
                self.note_link_read(value);
                self.walk_expr(value);
                if let StmtKind::LetElse { else_block, .. } = &stmt.kind {
                    self.walk_block(else_block);
                }
            }
            StmtKind::LetUninit { .. } => {}
            StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => self.walk_block(body),
            StmtKind::Assign { target, value } => {
                // A link store OUTSIDE the triple shape (loop or not).
                if let ExprKind::FieldAccess { object, field } = &target.kind {
                    if field == &self.scan.link_field {
                        if let ExprKind::Identifier(obj) = &object.kind {
                            if self.scan.kind_of(obj) == Some(ClusterKind::Bare) {
                                let value_name = match link_value_shape(value) {
                                    LinkValue::SomeIdent(v) => v.to_string(),
                                    // `= None` resets: treat as a store
                                    // site with no fresh value — it can
                                    // displace, so a non-triple loop
                                    // store of None also disqualifies
                                    // via rule 2; outside loops it
                                    // could orphan — disqualify by
                                    // making rule 1 fail.
                                    _ => String::new(),
                                };
                                self.stores.push(StoreSite {
                                    counter: self.counter,
                                    loop_depth: self.loop_depth,
                                    is_triple: false,
                                    loop_exit_counter: 0,
                                    value_name,
                                });
                            }
                        }
                    }
                }
                self.note_link_read(value);
                self.walk_expr(value);
            }
            StmtKind::CompoundAssign { value, .. } => {
                self.note_link_read(value);
                self.walk_expr(value);
            }
            StmtKind::Expr(e) => self.walk_expr(e),
        }
    }

    /// Link reads: `<bare>.link` appearing as a value (option-cursor
    /// creation). B1 already restricted them to let/assign RHS shapes.
    fn note_link_read(&mut self, value: &Expr) {
        if let ExprKind::FieldAccess { object, field } = &value.kind {
            if field == &self.scan.link_field {
                if let ExprKind::Identifier(n) = &object.kind {
                    if self.scan.kind_of(n) == Some(ClusterKind::Bare) {
                        self.reads.push(self.counter);
                    }
                }
            }
        }
    }

    fn walk_expr(&mut self, expr: &Expr) {
        match &expr.kind {
            ExprKind::Block(b)
            | ExprKind::Seq(b)
            | ExprKind::Unsafe(b)
            | ExprKind::Try(b)
            | ExprKind::Par(b)
            | ExprKind::Lock { body: b, .. }
            | ExprKind::LabeledBlock { body: b, .. } => self.walk_block(b),
            ExprKind::Loop { body: b, .. }
            | ExprKind::While { body: b, .. }
            | ExprKind::WhileLet { body: b, .. }
            | ExprKind::For { body: b, .. } => {
                self.loop_depth += 1;
                self.walk_block(b);
                self.loop_depth -= 1;
                // Patch any triple inside this loop with the exit
                // counter (the first counter value after the loop).
                let exit = self.counter;
                for s in &mut self.stores {
                    if s.is_triple && s.loop_exit_counter == 0 {
                        s.loop_exit_counter = exit;
                    }
                }
            }
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                self.walk_expr(condition);
                self.walk_block(then_block);
                if let Some(e) = else_branch {
                    self.walk_expr(e);
                }
            }
            ExprKind::IfLet {
                value,
                then_block,
                else_branch,
                ..
            } => {
                self.walk_expr(value);
                self.walk_block(then_block);
                if let Some(e) = else_branch {
                    self.walk_expr(e);
                }
            }
            ExprKind::Match { scrutinee, arms } => {
                self.walk_expr(scrutinee);
                for arm in arms {
                    self.walk_expr(&arm.body);
                }
            }
            ExprKind::Closure { body, .. } => self.walk_expr(body),
            ExprKind::Binary { left, right, .. } => {
                self.walk_expr(left);
                self.walk_expr(right);
            }
            ExprKind::Unary { operand, .. } => self.walk_expr(operand),
            ExprKind::Call { args, .. } => {
                for a in args {
                    self.walk_expr(&a.value);
                }
            }
            ExprKind::MethodCall { object, args, .. } => {
                self.walk_expr(object);
                for a in args {
                    self.walk_expr(&a.value);
                }
            }
            _ => {}
        }
    }
}
