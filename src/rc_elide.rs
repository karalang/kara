//! RC-elision safety analysis (default ON; opt out `KARAC_RC_ELIDE_REF_PARAMS=0`).
//!
//! The ownership pass classifies a parameter that is only read as
//! [`OwnershipMode::Ref`] — a borrow. For a `shared` / `Option[shared]` such
//! param, codegen normally still emits the caller-side retain (`rc_inc`) and the
//! callee-side scope-exit release (`rc_dec`). That pair is *balanced*: when the
//! caller keeps the referent alive across the call, deleting both removes real
//! RC traffic with zero net effect (the read-only tree walk in kata #101 — a
//! ~30% wall-time win). This module computes exactly which `ref` params are
//! **sound** to elide.
//!
//! Four conditions must all hold, or the balanced pair is NOT a no-op and
//! eliding it leaks (or double-frees). Conditions 3–4 are summarized here and
//! detailed at their code (`is_scalar_return` / `has_mut_out_param`; the
//! "Condition 4" section):
//!
//! 1. **Caller side — the arg is a borrow, and its referent outlives the call.**
//!    Every call must pass the param a *projection of a named binding* —
//!    `n.left`, `v[i]`, `t.0`, `self.head` — which READS a sub-value out of a
//!    container the caller still holds (a genuine borrow; the container keeps it
//!    alive and drops it at the caller's own scope exit) — **or** a *bare
//!    identifier naming a `Ref`-mode parameter of the enclosing function* (a
//!    **borrow-forward**: `is_balanced(root)` calling `check(root)` where `root`
//!    is `Ref`). A `Ref` param's referent is, by definition, kept alive by the
//!    enclosing frame for the whole call, so forwarding it is a genuine borrow
//!    regardless of whether the enclosing function itself elides — the same
//!    outlives-the-call guarantee a projection gives. A **bare** identifier
//!    naming an *owned local* (`let d = take(); eat(d)`) is still rejected: that
//!    is a *move* transferring `d`'s `+1` to the callee, whose exit dec is then
//!    load-bearing. A *fresh rvalue* (`Some(x)`, a call return) is rejected for
//!    the same reason. The borrow-forward relaxation is disabled inside closures
//!    (a captured borrow could outlive the call — fail-closed). The function must
//!    also be **directly called** at least once (arg shapes fully observed),
//!    never used as a **value** (indirect calls invisible), and not **`pub`**
//!    (external callers invisible).
//!
//! 2. **Callee side — the callee only borrows the param (never moves its
//!    resource out).** `OwnershipMode::Ref` is NOT enough: the ownership pass
//!    maps `let mut a = param` (a move-out) to `Read → Ref`, so a function that
//!    *transfers its param's nodes out* (`merge_two`, `merge_k`) is still `Ref`.
//!    Linux LSan proved this leaks. So we require the param to be **consumed in
//!    place** — used only as a direct `match`/`if let`/`while let` scrutinee (or
//!    unused), never bound to a `let`, assigned, returned, stored, or forwarded.
//!    This is exactly [`crate::result_escape::nonescaping_param_names`], the
//!    conservative, exhaustive, fail-closed non-escape analysis shipped for the
//!    `Result[shared]` scope-exit-dec residual (B-2026-07-12-24) — the same
//!    "is this binding's release safe to touch?" question. `is_mirror` (params
//!    used only as `match a` / `match b`) qualifies; `merge_two` / `merge_k`
//!    (their params appear as a `let mut a = l1` RHS) do not.
//!
//! 3. **No escape via return / output param.** A scalar (or unit) return type
//!    and no `mut ref` / `mut Slice` params — so a match-binding cannot leave
//!    via `return` (`insert`'s `Some(n)`) or a store into an outliving location.
//!
//! 4. **No escape via a payload moved out by value.** A match-*payload* of the
//!    param (its referent) may be read through a projection (`n.left`) or
//!    destructured further, but may not appear as a bare-identifier value —
//!    which could move the referent into a consuming callee. Sound by
//!    construction (`payloads_never_move_out`), independent of codegen re-share.
//!
//! The analysis is deliberately **conservative and fail-closed**: the caller
//! scan and the condition-4 pattern-binding collector are exhaustive `match`es
//! with no `_` arm (a new AST node breaks the build rather than silently
//! admitting an escape), and every param/payload use other than a scrutinee or
//! projection is treated as escaping. The worst case is a missed optimization,
//! never a leak. Codegen consumes the result via `borrowed_arg_skip` /
//! `borrowed_param_dec_skip`.

use crate::ast::{
    Block, Expr, ExprKind, Function, ImplItem, Item, ParsedInterpolationPart, Pattern, PatternKind,
    Program, RestPattern, Stmt, StmtKind, TraitItem, TypeExpr, TypeKind,
};
use crate::ownership::OwnershipMode;
use std::collections::{HashMap, HashSet};

/// A return type from which no `shared`/`Option[shared]` handle can escape — a
/// primitive scalar or unit. Anything else (an `Option`, a struct, a generic)
/// could carry the param's node out via `return`, so those functions are
/// excluded (the match-binding-return escape route — e.g. `insert`'s
/// `Some(n)`). Conservative: an `i64`-returning tree fold still qualifies.
fn is_scalar_return(rt: &Option<TypeExpr>) -> bool {
    let Some(te) = rt else {
        return true; // unit
    };
    match &te.kind {
        TypeKind::Path(p) => matches!(
            p.segments.first().map(String::as_str),
            Some(
                "i8" | "i16"
                    | "i32"
                    | "i64"
                    | "u8"
                    | "u16"
                    | "u32"
                    | "u64"
                    | "isize"
                    | "usize"
                    | "f32"
                    | "f64"
                    | "bool"
                    | "char"
            )
        ),
        _ => false,
    }
}

/// A `mut ref T` / `mut Slice[T]` parameter lets the callee store a
/// match-binding into a location that outlives the call (the store-into-output
/// escape route), so any such param excludes the whole function.
fn has_mut_out_param(func: &Function) -> bool {
    func.params
        .iter()
        .any(|p| matches!(&p.ty.kind, TypeKind::MutRef(_) | TypeKind::MutSlice(_)))
}

/// The root of a place chain is a named binding — `x`, `n.left`, `v[i]`, `t.0`,
/// `self`. `false` for anything rooted at a non-place (a call result,
/// `Some(..)`, a literal).
fn place_rooted_at_name(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Identifier(_) | ExprKind::SelfValue => true,
        ExprKind::FieldAccess { object, .. }
        | ExprKind::TupleIndex { object, .. }
        | ExprKind::Index { object, .. } => place_rooted_at_name(object),
        _ => false,
    }
}

/// A *projection* of a named binding — `n.left`, `v[i]`, `t.0`, `self.head` —
/// which READS a sub-value out of a container the caller still holds: a genuine
/// borrow (the container keeps the sub-value alive across the call, and drops it
/// at the caller's own scope exit). A **bare** `Identifier` / `self` is NOT a
/// projection: passing a whole binding by value is a *move/transfer* of it (an
/// owned local like `let d = take(); eat(d)` hands its `+1` to the callee, whose
/// exit dec is then load-bearing — eliding it leaks). A fresh rvalue (`Some(x)`,
/// a call return) is not a projection either. Only projections are safe to
/// hand a callee as an un-retained borrow.
fn is_borrow_projection(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::FieldAccess { object, .. }
        | ExprKind::TupleIndex { object, .. }
        | ExprKind::Index { object, .. } => place_rooted_at_name(object),
        _ => false,
    }
}

// ────────────────────────────────────────────────────────────────────────
// Caller-side candidate scan (condition 1)
// ────────────────────────────────────────────────────────────────────────

struct Scan<'a> {
    fn_names: &'a HashSet<&'a str>,
    /// Functions seen at least once as the direct callee of a
    /// `Call { callee: Identifier(name), .. }` — arg shapes fully observed.
    called: HashSet<String>,
    /// (callee, position) that some direct call passes a non-place argument to.
    unsafe_pos: HashMap<String, HashSet<usize>>,
    /// Function names used as a *value* (not a direct callee) — call sites not
    /// all visible, so no param may elide.
    escaped: HashSet<String>,
    /// The `Ref`-mode (read-only borrow) parameters of the function whose body
    /// is currently being walked. A bare-identifier argument naming one of these
    /// is a **borrow-forward**, not a move: a `Ref` param's referent is kept
    /// alive by the enclosing frame for the whole call (that is what borrow
    /// means), so handing it to a callee that borrows it can never observe
    /// refcount 0 mid-call — sound to treat as an un-retained borrow, exactly
    /// like a projection. Set per-body by the driver; cleared inside closures
    /// (a captured borrow could outlive the call), so the relaxation is
    /// fail-closed there.
    cur_ref_params: HashSet<String>,
}

impl Scan<'_> {
    fn walk_block(&mut self, block: &Block) {
        for stmt in &block.stmts {
            self.walk_stmt(stmt);
        }
        if let Some(tail) = &block.final_expr {
            self.walk_expr(tail);
        }
    }

    fn walk_stmt(&mut self, stmt: &Stmt) {
        match &stmt.kind {
            StmtKind::Let { value, .. } | StmtKind::Expr(value) => self.walk_expr(value),
            StmtKind::LetElse {
                value, else_block, ..
            } => {
                self.walk_expr(value);
                self.walk_block(else_block);
            }
            StmtKind::LetUninit { .. } => {}
            StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => self.walk_block(body),
            StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
                self.walk_expr(target);
                self.walk_expr(value);
            }
            StmtKind::MultiAssign { targets, values } => {
                for t in targets {
                    self.walk_expr(t);
                }
                for v in values {
                    self.walk_expr(v);
                }
            }
        }
    }

    fn walk_expr(&mut self, expr: &Expr) {
        match &expr.kind {
            ExprKind::Call { callee, args } => {
                let direct = match &callee.kind {
                    ExprKind::Identifier(name) if self.fn_names.contains(name.as_str()) => {
                        self.called.insert(name.clone());
                        for (i, arg) in args.iter().enumerate() {
                            if !is_borrow_projection(&arg.value)
                                && !self.is_ref_param_forward(&arg.value)
                            {
                                self.unsafe_pos.entry(name.clone()).or_default().insert(i);
                            }
                        }
                        true
                    }
                    _ => false,
                };
                if !direct {
                    self.walk_expr(callee);
                }
                for arg in args {
                    self.walk_expr(&arg.value);
                }
            }
            ExprKind::Identifier(name) => {
                if self.fn_names.contains(name.as_str()) {
                    self.escaped.insert(name.clone());
                }
            }
            ExprKind::Closure { body, .. } => {
                // A captured `Ref` param could be forwarded from a closure that
                // outlives the borrow, so the borrow-forward relaxation is unsafe
                // here: clear the ref-param set while walking the closure body
                // (fail-closed — a bare-identifier arg inside reverts to a move).
                let saved = std::mem::take(&mut self.cur_ref_params);
                self.walk_expr(body);
                self.cur_ref_params = saved;
            }
            other => {
                let mut recur = |e: &Expr| self.walk_expr(e);
                walk_children(other, &mut recur);
            }
        }
    }

    /// `true` if `expr` is a bare identifier naming a `Ref`-mode parameter of the
    /// function currently being walked — a borrow-forward (see `cur_ref_params`).
    fn is_ref_param_forward(&self, expr: &Expr) -> bool {
        matches!(&expr.kind, ExprKind::Identifier(n) if self.cur_ref_params.contains(n.as_str()))
    }
}

/// Generic child-expression visitor — exhaustive over `ExprKind` (no `_`), used
/// by the caller-side scan for any node other than `Call` / `Identifier`.
fn walk_children(kind: &ExprKind, f: &mut dyn FnMut(&Expr)) {
    fn blk(b: &Block, f: &mut dyn FnMut(&Expr)) {
        for s in &b.stmts {
            walk_stmt_children(s, f);
        }
        if let Some(e) = &b.final_expr {
            f(e);
        }
    }
    match kind {
        ExprKind::Integer(..)
        | ExprKind::Float(..)
        | ExprKind::CharLit(..)
        | ExprKind::ByteLit(..)
        | ExprKind::StringLit(..)
        | ExprKind::MultiStringLit(..)
        | ExprKind::CStringLit { .. }
        | ExprKind::Bool(..)
        | ExprKind::Identifier(..)
        | ExprKind::Path { .. }
        | ExprKind::SelfValue
        | ExprKind::SelfType
        | ExprKind::PipePlaceholder
        | ExprKind::OffsetOf { .. }
        | ExprKind::Continue { .. }
        | ExprKind::Error => {}
        ExprKind::InterpolatedStringLit(parts) => {
            for p in parts {
                if let ParsedInterpolationPart::Expr(e, _) = p {
                    f(e);
                }
            }
        }
        ExprKind::Binary { left, right, .. }
        | ExprKind::NilCoalesce { left, right }
        | ExprKind::Pipe { left, right } => {
            f(left);
            f(right);
        }
        ExprKind::Unary { operand, .. } => f(operand),
        ExprKind::Question(inner) => f(inner),
        ExprKind::Cast { expr, .. } => f(expr),
        ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => f(object),
        ExprKind::Index { object, index } => {
            f(object);
            f(index);
        }
        ExprKind::Call { callee, args } => {
            f(callee);
            for a in args {
                f(&a.value);
            }
        }
        ExprKind::MethodCall { object, args, .. } => {
            f(object);
            for a in args {
                f(&a.value);
            }
        }
        ExprKind::OptionalChain { object, args, .. } => {
            f(object);
            if let Some(args) = args {
                for a in args {
                    f(&a.value);
                }
            }
        }
        ExprKind::Block(b)
        | ExprKind::Comptime(b)
        | ExprKind::Unsafe(b)
        | ExprKind::Try(b)
        | ExprKind::Seq(b)
        | ExprKind::Par(b) => blk(b, f),
        ExprKind::LabeledBlock { body, .. } | ExprKind::Loop { body, .. } => blk(body, f),
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            f(condition);
            blk(then_block, f);
            if let Some(e) = else_branch {
                f(e);
            }
        }
        ExprKind::IfLet {
            value,
            then_block,
            else_branch,
            ..
        } => {
            f(value);
            blk(then_block, f);
            if let Some(e) = else_branch {
                f(e);
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            f(scrutinee);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    f(g);
                }
                f(&arm.body);
            }
        }
        ExprKind::While {
            condition, body, ..
        } => {
            f(condition);
            blk(body, f);
        }
        ExprKind::WhileLet { value, body, .. } => {
            f(value);
            blk(body, f);
        }
        ExprKind::For { iterable, body, .. } => {
            f(iterable);
            blk(body, f);
        }
        ExprKind::Closure { body, .. } => f(body),
        ExprKind::Return(inner) => {
            if let Some(e) = inner {
                f(e);
            }
        }
        ExprKind::Break { value, .. } => {
            if let Some(e) = value {
                f(e);
            }
        }
        ExprKind::Tuple(elems) | ExprKind::ArrayLiteral(elems) => {
            for e in elems {
                f(e);
            }
        }
        ExprKind::PrefixCollectionLiteral { items, .. } => {
            for e in items {
                f(e);
            }
        }
        ExprKind::RepeatLiteral { value, count, .. } => {
            f(value);
            f(count);
        }
        ExprKind::MapLiteral(pairs) => {
            for (k, v) in pairs {
                f(k);
                f(v);
            }
        }
        ExprKind::StructLiteral { fields, spread, .. } => {
            for fld in fields {
                f(&fld.value);
            }
            if let Some(s) = spread {
                f(s);
            }
        }
        ExprKind::Range { start, end, .. } => {
            if let Some(e) = start {
                f(e);
            }
            if let Some(e) = end {
                f(e);
            }
        }
        ExprKind::Lock { mutex, body, .. } => {
            f(mutex);
            blk(body, f);
        }
        ExprKind::Providers { bindings, body } => {
            for b in bindings {
                f(&b.value);
            }
            blk(body, f);
        }
    }
}

fn walk_stmt_children(stmt: &Stmt, f: &mut dyn FnMut(&Expr)) {
    match &stmt.kind {
        StmtKind::Let { value, .. } | StmtKind::Expr(value) => f(value),
        StmtKind::LetElse {
            value, else_block, ..
        } => {
            f(value);
            for s in &else_block.stmts {
                walk_stmt_children(s, f);
            }
            if let Some(e) = &else_block.final_expr {
                f(e);
            }
        }
        StmtKind::LetUninit { .. } => {}
        StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
            for s in &body.stmts {
                walk_stmt_children(s, f);
            }
            if let Some(e) = &body.final_expr {
                f(e);
            }
        }
        StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
            f(target);
            f(value);
        }
        StmtKind::MultiAssign { targets, values } => {
            for t in targets {
                f(t);
            }
            for v in values {
                f(v);
            }
        }
    }
}

// ────────────────────────────────────────────────────────────────────────
// Condition 4 — payload-escape guard (syntactic; callee-consume-independent)
// ────────────────────────────────────────────────────────────────────────
//
// Conditions 1–3 leave one route open (docs/spikes/rc-elide-ref-params.md,
// "Known residual"): a match-binding of the elided param passed BY VALUE to a
// consuming callee — `match p { Some(n) => consume(n) }`. Linux LSan shows that
// route is *balanced today* (codegen re-shares the borrowed payload, so the
// consumer's dec pairs with its own inc, never touching `p`'s elided retain) —
// but that safety rests on the re-share codegen invariant, not on this analysis.
//
// This condition makes elision sound BY CONSTRUCTION, independent of any
// callee's or codegen's behavior: a param's match-payload (its referent) may be
// READ through a projection (`n.left` — a borrow of a *sub*-node) or
// destructured further (a nested `match`/`if let` scrutinee), but it may NOT
// appear as a **bare-identifier value** — the only shape that could move the
// referent itself into a consuming position. So eliding `p`'s retain/release can
// never be unbalanced by what happens to its payload.
//
// `is_mirror` / `is_symmetric` (the #101 win) qualify: their payloads `an` /
// `bn` / `n` appear ONLY as `an.left` / `n.right` projection roots, never bare.
// `probe`-style consumers (`Some(n) => sink(n)`) are rejected. Conservative and
// fail-closed: uncertain positions count as an escape (a missed optimization,
// never a leak); the pattern-binding collector is an exhaustive `match`.

/// Names a pattern binds into arm scope. Exhaustive over `PatternKind` — a new
/// pattern node breaks the build rather than silently dropping a payload binding.
fn collect_pattern_bindings(pat: &Pattern, out: &mut HashSet<String>) {
    match &pat.kind {
        PatternKind::Wildcard | PatternKind::Literal(_) | PatternKind::RangePattern { .. } => {}
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
                    // `S { x }` field shorthand binds `x`.
                    None => {
                        out.insert(f.name.clone());
                    }
                }
            }
        }
        PatternKind::TupleVariant { patterns, .. } => {
            for p in patterns {
                collect_pattern_bindings(p, out);
            }
        }
        PatternKind::Tuple(patterns) | PatternKind::Or(patterns) => {
            for p in patterns {
                collect_pattern_bindings(p, out);
            }
        }
        PatternKind::Slice {
            prefix,
            rest,
            suffix,
        } => {
            for p in prefix {
                collect_pattern_bindings(p, out);
            }
            if let Some(RestPattern::Bound(name)) = rest {
                out.insert(name.clone());
            }
            for p in suffix {
                collect_pattern_bindings(p, out);
            }
        }
    }
}

/// Single pre-order walk that BOTH grows the payload-lineage set of `param` and
/// flags any bare-identifier use of a member of it. Pre-order is what makes one
/// pass sufficient: a payload binding is added when its `match`/`if let`
/// scrutinee is entered, strictly before the arm body (where its uses live) is
/// walked, so nested destructuring extends the lineage before its own uses are
/// checked. All blocks are routed through [`Self::block`] so statement-level
/// constructs (esp. a refutable `let … else`) are never skipped.
struct PayloadScan<'a> {
    param: &'a str,
    derived: HashSet<String>,
    /// Set once a bare-identifier move of a lineage member is seen.
    bad: bool,
    /// Inside a closure body, referencing a lineage member is a CAPTURE (an
    /// escape into an env that can outlive the borrow) — so even a projection
    /// read or a nested scrutinee counts as an escape there.
    in_closure: bool,
}

impl PayloadScan<'_> {
    /// `true` if `e` is a bare identifier naming the param or an existing
    /// lineage member — a consume-in-place scrutinee that extends the lineage.
    fn tracked_scrutinee(&self, e: &Expr) -> bool {
        match &e.kind {
            ExprKind::Identifier(n) => {
                n.as_str() == self.param || self.derived.contains(n.as_str())
            }
            _ => false,
        }
    }

    /// Record a lineage scrutinee: collect its pattern's bindings; a capture
    /// (inside a closure) is an escape.
    fn note_lineage_scrutinee(&mut self, pat: &Pattern) {
        if self.in_closure {
            self.bad = true;
        }
        collect_pattern_bindings(pat, &mut self.derived);
    }

    fn block(&mut self, b: &Block) {
        for s in &b.stmts {
            self.stmt(s);
        }
        if let Some(e) = &b.final_expr {
            self.expr(e);
        }
    }

    fn stmt(&mut self, s: &Stmt) {
        match &s.kind {
            StmtKind::Let { value, .. } | StmtKind::Expr(value) => self.expr(value),
            StmtKind::LetElse {
                pattern,
                value,
                else_block,
                ..
            } => {
                // Refutable `let Pat = <scrutinee> else` is match-sugar: a
                // lineage scrutinee is consumed in place (not a bare move).
                if self.tracked_scrutinee(value) {
                    self.note_lineage_scrutinee(pattern);
                } else {
                    self.expr(value);
                }
                self.block(else_block);
            }
            StmtKind::LetUninit { .. } => {}
            StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => self.block(body),
            StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
                self.expr(target);
                self.expr(value);
            }
            StmtKind::MultiAssign { targets, values } => {
                for t in targets {
                    self.expr(t);
                }
                for v in values {
                    self.expr(v);
                }
            }
        }
    }

    /// Walk the object side of a projection chain (`a.b.c`, `v[i]`). A named root
    /// being READ is a borrow of a sub-value — allowed (outside a closure). Only
    /// the non-root pieces (an index expr, a non-place root like `f(x).bar`) are
    /// ordinary use positions.
    fn proj_object(&mut self, obj: &Expr) {
        match &obj.kind {
            ExprKind::Identifier(n) => {
                if self.in_closure && self.derived.contains(n.as_str()) {
                    self.bad = true;
                }
            }
            ExprKind::SelfValue => {}
            ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
                self.proj_object(object);
            }
            ExprKind::Index { object, index } => {
                self.proj_object(object);
                self.expr(index);
            }
            // A non-place root (call result, block-expr, literal, …) is not a
            // borrow projection; recurse so any buried lineage member is caught.
            _ => self.expr(obj),
        }
    }

    fn expr(&mut self, e: &Expr) {
        if self.bad {
            return;
        }
        match &e.kind {
            // A bare-identifier value use of a lineage member is the move-out we
            // reject (the residual). Projection roots / scrutinees are intercepted
            // before reaching here, so anything landing here is a bare value.
            ExprKind::Identifier(n) => {
                if self.derived.contains(n.as_str()) {
                    self.bad = true;
                }
            }
            ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
                self.proj_object(object);
            }
            ExprKind::Index { object, index } => {
                self.proj_object(object);
                self.expr(index);
            }
            ExprKind::Match { scrutinee, arms } => {
                if self.tracked_scrutinee(scrutinee) {
                    for arm in arms {
                        self.note_lineage_scrutinee(&arm.pattern);
                    }
                } else {
                    self.expr(scrutinee);
                }
                for arm in arms {
                    if let Some(g) = &arm.guard {
                        self.expr(g);
                    }
                    self.expr(&arm.body);
                }
            }
            ExprKind::IfLet {
                pattern,
                value,
                then_block,
                else_branch,
            } => {
                if self.tracked_scrutinee(value) {
                    self.note_lineage_scrutinee(pattern);
                } else {
                    self.expr(value);
                }
                self.block(then_block);
                if let Some(el) = else_branch {
                    self.expr(el);
                }
            }
            ExprKind::WhileLet {
                pattern,
                value,
                body,
                ..
            } => {
                if self.tracked_scrutinee(value) {
                    self.note_lineage_scrutinee(pattern);
                } else {
                    self.expr(value);
                }
                self.block(body);
            }
            // Block-containing exprs: route blocks through `self.block` so
            // stmt-level constructs (refutable let-else) are always seen.
            ExprKind::Block(b)
            | ExprKind::Comptime(b)
            | ExprKind::Unsafe(b)
            | ExprKind::Try(b)
            | ExprKind::Seq(b)
            | ExprKind::Par(b) => self.block(b),
            ExprKind::LabeledBlock { body, .. } | ExprKind::Loop { body, .. } => self.block(body),
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                self.expr(condition);
                self.block(then_block);
                if let Some(el) = else_branch {
                    self.expr(el);
                }
            }
            ExprKind::While {
                condition, body, ..
            } => {
                self.expr(condition);
                self.block(body);
            }
            ExprKind::For { iterable, body, .. } => {
                self.expr(iterable);
                self.block(body);
            }
            ExprKind::Lock { mutex, body, .. } => {
                self.expr(mutex);
                self.block(body);
            }
            ExprKind::Providers { bindings, body } => {
                for b in bindings {
                    self.expr(&b.value);
                }
                self.block(body);
            }
            ExprKind::Closure { body, .. } => {
                let prev = self.in_closure;
                self.in_closure = true;
                self.expr(body);
                self.in_closure = prev;
            }
            // No blocks, no special positions — recurse into sub-exprs, treating
            // each bare identifier reached as an ordinary use.
            other => {
                walk_children(other, &mut |e| self.expr(e));
            }
        }
    }
}

/// Condition 4: every match-payload of `param` is used only as a projection root
/// or a nested scrutinee — never moved out as a bare-identifier value. See the
/// section header for the soundness argument.
fn payloads_never_move_out(func: &Function, param: &str) -> bool {
    let mut scan = PayloadScan {
        param,
        derived: HashSet::new(),
        bad: false,
        in_closure: false,
    };
    scan.block(&func.body);
    !scan.bad
}

// ────────────────────────────────────────────────────────────────────────
// Driver
// ────────────────────────────────────────────────────────────────────────

/// Compute the `ref` parameters that are sound to RC-elide (see module docs):
/// function name → `(param_name, position)`.
pub fn safe_elidable_ref_params(
    program: &Program,
    param_modes: &HashMap<String, Vec<(String, OwnershipMode)>>,
) -> HashMap<String, Vec<(String, usize)>> {
    let fn_names: HashSet<&str> = param_modes.keys().map(|s| s.as_str()).collect();

    // Per-function `Ref`-mode (read-only borrow) parameter names — the set a
    // bare-identifier argument may forward as a borrow (condition-1 relaxation).
    let ref_params_of = |fname: &str| -> HashSet<String> {
        param_modes
            .get(fname)
            .map(|ms| {
                ms.iter()
                    .filter(|(_, m)| matches!(m, OwnershipMode::Ref))
                    .map(|(n, _)| n.clone())
                    .collect()
            })
            .unwrap_or_default()
    };

    // Condition 1 — caller-side scan over EVERY function/method body. Before each
    // body, `cur_ref_params` is set to that function's `Ref` params so a
    // borrow-forward (`helper(root)` where `root` is a `Ref` param) is not
    // mis-scored as a move.
    let mut scan = Scan {
        fn_names: &fn_names,
        called: HashSet::new(),
        unsafe_pos: HashMap::new(),
        escaped: HashSet::new(),
        cur_ref_params: HashSet::new(),
    };
    // Free functions the ownership pass knows about, by name (only these are
    // directly-called elision candidates; methods are excluded via `called`).
    let mut free_fns: HashMap<&str, &Function> = HashMap::new();
    for item in &program.items {
        match item {
            Item::Function(f) => {
                scan.cur_ref_params = ref_params_of(&f.name);
                scan.walk_block(&f.body);
                free_fns.insert(f.name.as_str(), f);
            }
            Item::ImplBlock(b) => {
                for inner in &b.items {
                    if let ImplItem::Method(m) = inner {
                        // Methods aren't keyed in `param_modes` by a bare name, so
                        // fall back to the empty set (conservative — no forward
                        // relaxation from a method body).
                        scan.cur_ref_params = ref_params_of(&m.name);
                        scan.walk_block(&m.body);
                    }
                }
            }
            Item::TraitDef(t) => {
                for inner in &t.items {
                    if let TraitItem::Method(m) = inner {
                        if let Some(body) = &m.body {
                            scan.cur_ref_params = ref_params_of(&m.name);
                            scan.walk_block(body);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    let mut out: HashMap<String, Vec<(String, usize)>> = HashMap::new();
    for (fname, modes) in param_modes {
        // Condition-1 function-level filters.
        let Some(func) = free_fns.get(fname.as_str()) else {
            continue;
        };
        if func.is_pub
            || !scan.called.contains(fname)
            || scan.escaped.contains(fname)
            || !is_scalar_return(&func.return_type)
            || has_mut_out_param(func)
        {
            continue;
        }
        // Condition 2 — callee-side: the param is consumed in place (used only as
        // a match/if-let scrutinee, never moved out). Reuses the shipped
        // `result_escape` non-escape analysis.
        let nonescaping = crate::result_escape::nonescaping_param_names(func);
        let bad = scan.unsafe_pos.get(fname);
        let recs: Vec<(String, usize)> = modes
            .iter()
            .enumerate()
            .filter(|(i, (name, m))| {
                matches!(m, OwnershipMode::Ref)
                    && !bad.is_some_and(|s| s.contains(i))
                    && nonescaping.contains(name)
                    // Condition 4 — the param's match-payloads never move out as a
                    // bare-identifier value (sound by construction; see section).
                    && payloads_never_move_out(func, name)
            })
            .map(|(i, (n, _))| (n.clone(), i))
            .collect();
        if !recs.is_empty() {
            out.insert(fname.clone(), recs);
        }
    }

    // Condition 5 (B-2026-07-16-7) — the elided call web must be hermetically
    // READ-ONLY over shared state. Conditions 1–4 never constrained WHERE a
    // payload projection may flow: an elided fn's arm could pass `n.back` (any
    // projection) to an arbitrary callee, and on an up/back-pointer graph that
    // callee — following the ordinary owned protocol, so its own counts
    // balance — could field-assign through the alias (`m.left = None`) and
    // release the ONLY count keeping the borrowed node alive (an elided frame
    // holds no +1 of its own). Reproduced as a silent wrong answer + valgrind
    // invalid-read on the make/walk/detach probe (ledger entry). The fix is a
    // fixed-point refinement over the condition-1–4 set: drop any fn that
    // (5a) contains a projection-target store or a lineage-rooted method
    // call, (5b) has a shared-carrying param that is not itself elided, or
    // (5c) lets a lineage projection (or a let-alias of one — those join the
    // lineage) flow anywhere except an elided position of a surviving fn or
    // a nested destructure. Removal cascades: a dropped callee turns its
    // callers' projection-args into non-sanctioned flows on the next pass.
    let carriers = collect_shared_carrying_types(program);
    let field_class = classify_struct_fields(program, &carriers);
    loop {
        let names: Vec<String> = out.keys().cloned().collect();
        let mut removed = false;
        for fname in names {
            let func = free_fns[fname.as_str()];
            let recs = &out[&fname];
            if !condition5_ok(func, recs, &out, &carriers, &field_class) {
                if std::env::var_os("KARAC_RC_ELIDE_DEBUG").is_some() {
                    eprintln!("[rc-elide] condition 5 drops: {fname}");
                }
                out.remove(&fname);
                removed = true;
            }
        }
        if !removed {
            break;
        }
    }

    if std::env::var_os("KARAC_RC_ELIDE_DEBUG").is_some() {
        eprintln!("[rc-elide] elidable set: {out:?}");
    }
    out
}

// ────────────────────────────────────────────────────────────────────────
// Condition 5 — read-only elided web (B-2026-07-16-7)
// ────────────────────────────────────────────────────────────────────────

/// Every type name that can (transitively) hold a `shared`/`par` handle: the
/// shared/par struct+enum names themselves, plus any struct/enum with a field
/// or variant whose declared type mentions a member of the set (fixpoint).
/// Drives 5b (an owned param of such a type is a potential alias of the
/// borrowed graph) and the field classification below.
fn collect_shared_carrying_types(program: &Program) -> HashSet<String> {
    let mut set: HashSet<String> = HashSet::new();
    for item in &program.items {
        match item {
            Item::StructDef(s) if s.is_shared || s.is_par => {
                set.insert(s.name.clone());
            }
            Item::EnumDef(e) if e.is_shared || e.is_par => {
                set.insert(e.name.clone());
            }
            _ => {}
        }
    }
    loop {
        let mut grew = false;
        for item in &program.items {
            match item {
                Item::StructDef(s) if !set.contains(&s.name) => {
                    if s.fields
                        .iter()
                        .any(|f| type_shared_payload(&f.ty, &set).is_some())
                    {
                        set.insert(s.name.clone());
                        grew = true;
                    }
                }
                Item::EnumDef(e) if !set.contains(&e.name) => {
                    let mentions = e.variants.iter().any(|v| match &v.kind {
                        crate::ast::VariantKind::Unit => false,
                        crate::ast::VariantKind::Tuple(tes) => {
                            tes.iter().any(|te| type_shared_payload(te, &set).is_some())
                        }
                        crate::ast::VariantKind::Struct(fields) => fields
                            .iter()
                            .any(|f| type_shared_payload(&f.ty, &set).is_some()),
                    });
                    if mentions {
                        set.insert(e.name.clone());
                        grew = true;
                    }
                }
                _ => {}
            }
        }
        if !grew {
            break;
        }
    }
    set
}

/// First shared-carrying type name mentioned anywhere in `te` (path segments
/// and generic args, recursively), or `None` when the type provably holds no
/// handle. Exhaustive over `TypeKind` — a new type form fails the build here
/// rather than silently classifying as handle-free.
fn type_shared_payload(te: &TypeExpr, carriers: &HashSet<String>) -> Option<String> {
    fn path(p: &crate::ast::PathExpr, carriers: &HashSet<String>) -> Option<String> {
        for seg in &p.segments {
            if carriers.contains(seg.as_str()) {
                return Some(seg.clone());
            }
        }
        if let Some(args) = &p.generic_args {
            for a in args {
                if let crate::ast::GenericArg::Type(t) = a {
                    if let Some(n) = type_shared_payload(t, carriers) {
                        return Some(n);
                    }
                }
            }
        }
        None
    }
    match &te.kind {
        TypeKind::Path(p) => path(p, carriers),
        TypeKind::Tuple(ts) => ts.iter().find_map(|t| type_shared_payload(t, carriers)),
        TypeKind::Array { element, .. } => type_shared_payload(element, carriers),
        TypeKind::Pointer { inner, .. }
        | TypeKind::Ref(inner)
        | TypeKind::MutRef(inner)
        | TypeKind::MutSlice(inner)
        | TypeKind::Weak(inner) => type_shared_payload(inner, carriers),
        TypeKind::FnType {
            params,
            return_type,
            ..
        } => params
            .iter()
            .find_map(|t| type_shared_payload(t, carriers))
            .or_else(|| {
                return_type
                    .as_ref()
                    .and_then(|t| type_shared_payload(t, carriers))
            }),
        // A trait object / opaque type could box a shared handle behind an
        // interface — treat as shared-carrying (fail closed, drives 5b).
        TypeKind::Dyn { .. } | TypeKind::ImplTrait { .. } => Some("dyn".to_string()),
        TypeKind::Unit | TypeKind::Error => None,
    }
}

/// `(struct, field)` → `Some(carrier)` when the field's declared type can hold
/// a shared handle (a RESTRICTED projection under 5c), `None` for a provably
/// handle-free field (a FREE scalar-ish read). Covers every struct — lineage
/// can pass through a plain struct that carries shared handles.
fn classify_struct_fields(
    program: &Program,
    carriers: &HashSet<String>,
) -> HashMap<(String, String), Option<String>> {
    let mut out = HashMap::new();
    for item in &program.items {
        if let Item::StructDef(s) = item {
            for f in &s.fields {
                out.insert(
                    (s.name.clone(), f.name.clone()),
                    type_shared_payload(&f.ty, carriers),
                );
            }
        }
    }
    out
}

/// How a place-expression relates to the borrowed lineage.
enum PlaceClass<'e> {
    /// Not rooted at a lineage binding (or not a place at all) — walk normally.
    NotLineage,
    /// Rooted at lineage but every handle-bearing hop bottoms out in a
    /// provably handle-free field — the VALUE is a scalar-ish copy, free to
    /// use anywhere. Carries the index/argument sub-expressions that still
    /// need an ordinary walk.
    Free(Vec<&'e Expr>),
    /// A live handle into the borrowed graph (payload struct name): legal
    /// ONLY as an elided-position arg, a nested scrutinee, or a plain-`let`
    /// RHS (the alias joins the lineage).
    Restricted(String, Vec<&'e Expr>),
    /// Lineage-rooted but unresolvable (unknown field, tuple hop, enum
    /// struct) — fail closed.
    Poison,
}

/// Condition-5 body walk for one elidable fn. `bad` latches on the first
/// violation; the surrounding fixed point then removes the fn (and re-checks
/// its callers on the next iteration).
struct Cond5<'a> {
    elided: &'a HashMap<String, Vec<(String, usize)>>,
    fields: &'a HashMap<(String, String), Option<String>>,
    /// binding → payload struct name it can reach.
    lineage: HashMap<String, String>,
    bad: bool,
    in_closure: bool,
}

impl<'e> Cond5<'_> {
    fn resolve(&mut self, e: &'e Expr) -> PlaceClass<'e> {
        match &e.kind {
            ExprKind::Identifier(n) => match self.lineage.get(n.as_str()) {
                Some(t) => {
                    if self.in_closure {
                        self.bad = true;
                    }
                    PlaceClass::Restricted(t.clone(), Vec::new())
                }
                None => PlaceClass::NotLineage,
            },
            ExprKind::FieldAccess { object, field } => match self.resolve(object) {
                PlaceClass::NotLineage => PlaceClass::NotLineage,
                PlaceClass::Free(subs) => PlaceClass::Free(subs),
                PlaceClass::Restricted(t, subs) => {
                    match self.fields.get(&(t, field.clone())) {
                        Some(None) => PlaceClass::Free(subs),
                        Some(Some(u)) => PlaceClass::Restricted(u.clone(), subs),
                        // Unknown (struct without a def entry — an enum
                        // payload, a builtin) — fail closed.
                        None => PlaceClass::Poison,
                    }
                }
                PlaceClass::Poison => PlaceClass::Poison,
            },
            ExprKind::Index { object, index } => match self.resolve(object) {
                PlaceClass::NotLineage => PlaceClass::NotLineage,
                PlaceClass::Free(mut subs) => {
                    subs.push(index);
                    PlaceClass::Free(subs)
                }
                PlaceClass::Restricted(t, mut subs) => {
                    // Indexing a handle-bearing container (`n.kids[i]`)
                    // yields another handle of the same carrier family.
                    subs.push(index);
                    PlaceClass::Restricted(t, subs)
                }
                PlaceClass::Poison => PlaceClass::Poison,
            },
            ExprKind::TupleIndex { object, .. } => match self.resolve(object) {
                PlaceClass::NotLineage => PlaceClass::NotLineage,
                PlaceClass::Free(subs) => PlaceClass::Free(subs),
                // No per-position tuple typing here — fail closed.
                PlaceClass::Restricted(..) => PlaceClass::Poison,
                PlaceClass::Poison => PlaceClass::Poison,
            },
            _ => PlaceClass::NotLineage,
        }
    }

    /// A restricted place reached a non-sanctioned position?
    fn ctx_expr(&mut self, e: &'e Expr) {
        if self.bad {
            return;
        }
        match &e.kind {
            ExprKind::Call { callee, args } => {
                let callee_name = match &callee.kind {
                    ExprKind::Identifier(n) => Some(n.as_str()),
                    _ => None,
                };
                for (j, arg) in args.iter().enumerate() {
                    match self.resolve(&arg.value) {
                        PlaceClass::Restricted(_, subs) => {
                            let sanctioned = callee_name.is_some_and(|g| {
                                self.elided
                                    .get(g)
                                    .is_some_and(|recs| recs.iter().any(|(_, pos)| *pos == j))
                            });
                            if !sanctioned {
                                self.bad = true;
                                return;
                            }
                            for s in subs {
                                self.ctx_expr(s);
                            }
                        }
                        PlaceClass::Poison => {
                            self.bad = true;
                            return;
                        }
                        PlaceClass::Free(subs) => {
                            for s in subs {
                                self.ctx_expr(s);
                            }
                        }
                        PlaceClass::NotLineage => self.ctx_expr(&arg.value),
                    }
                }
                if callee_name.is_none() {
                    self.ctx_expr(callee);
                }
            }
            ExprKind::MethodCall { object, args, .. } => {
                // 5a method leg: a lineage-rooted receiver could mutate the
                // borrowed graph (or hand out an alias) — ban it outright.
                match self.resolve(object) {
                    PlaceClass::Restricted(..) | PlaceClass::Poison => {
                        self.bad = true;
                        return;
                    }
                    PlaceClass::Free(subs) => {
                        for s in subs {
                            self.ctx_expr(s);
                        }
                    }
                    PlaceClass::NotLineage => self.ctx_expr(object),
                }
                for arg in args {
                    match self.resolve(&arg.value) {
                        PlaceClass::Restricted(..) | PlaceClass::Poison => {
                            self.bad = true;
                            return;
                        }
                        PlaceClass::Free(subs) => {
                            for s in subs {
                                self.ctx_expr(s);
                            }
                        }
                        PlaceClass::NotLineage => self.ctx_expr(&arg.value),
                    }
                }
            }
            ExprKind::Match { scrutinee, arms } => {
                match self.resolve(scrutinee) {
                    PlaceClass::Restricted(u, subs) => {
                        for s in subs {
                            self.ctx_expr(s);
                        }
                        for arm in arms {
                            let mut names = HashSet::new();
                            collect_pattern_bindings(&arm.pattern, &mut names);
                            for n in names {
                                self.lineage.insert(n, u.clone());
                            }
                        }
                    }
                    PlaceClass::Poison => {
                        self.bad = true;
                        return;
                    }
                    PlaceClass::Free(subs) => {
                        for s in subs {
                            self.ctx_expr(s);
                        }
                    }
                    PlaceClass::NotLineage => self.ctx_expr(scrutinee),
                }
                for arm in arms {
                    if let Some(g) = &arm.guard {
                        self.ctx_expr(g);
                    }
                    self.ctx_expr(&arm.body);
                }
            }
            ExprKind::IfLet {
                pattern,
                value,
                then_block,
                else_branch,
            } => {
                self.destructure(pattern, value);
                self.block(then_block);
                if let Some(el) = else_branch {
                    self.ctx_expr(el);
                }
            }
            ExprKind::WhileLet {
                pattern,
                value,
                body,
                ..
            } => {
                self.destructure(pattern, value);
                self.block(body);
            }
            ExprKind::Closure { body, .. } => {
                let prev = self.in_closure;
                self.in_closure = true;
                self.ctx_expr(body);
                self.in_closure = prev;
            }
            ExprKind::Block(b)
            | ExprKind::Comptime(b)
            | ExprKind::Unsafe(b)
            | ExprKind::Try(b)
            | ExprKind::Seq(b)
            | ExprKind::Par(b) => self.block(b),
            ExprKind::LabeledBlock { body, .. } | ExprKind::Loop { body, .. } => self.block(body),
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                self.ctx_expr(condition);
                self.block(then_block);
                if let Some(el) = else_branch {
                    self.ctx_expr(el);
                }
            }
            ExprKind::While {
                condition, body, ..
            } => {
                self.ctx_expr(condition);
                self.block(body);
            }
            ExprKind::For { iterable, body, .. } => {
                // Iterating the borrowed graph hands the loop variable a
                // handle without a lineage entry — fail closed on a
                // lineage-rooted iterable.
                match self.resolve(iterable) {
                    PlaceClass::Restricted(..) | PlaceClass::Poison => {
                        self.bad = true;
                        return;
                    }
                    PlaceClass::Free(subs) => {
                        for s in subs {
                            self.ctx_expr(s);
                        }
                    }
                    PlaceClass::NotLineage => self.ctx_expr(iterable),
                }
                self.block(body);
            }
            ExprKind::Lock { mutex, body, .. } => {
                self.ctx_expr(mutex);
                self.block(body);
            }
            ExprKind::Providers { bindings, body } => {
                for b in bindings {
                    self.ctx_expr(&b.value);
                }
                self.block(body);
            }
            _ => match self.resolve(e) {
                PlaceClass::Restricted(..) | PlaceClass::Poison => {
                    self.bad = true;
                }
                PlaceClass::Free(subs) => {
                    for s in subs {
                        self.ctx_expr(s);
                    }
                }
                PlaceClass::NotLineage => {
                    walk_children(&e.kind, &mut |c| self.ctx_expr(c));
                }
            },
        }
    }

    /// Pattern-binding against a possibly-restricted scrutinee (if-let /
    /// while-let / let-else): bindings under a restricted scrutinee join the
    /// lineage; otherwise the value is walked normally.
    fn destructure(&mut self, pattern: &Pattern, value: &'e Expr) {
        match self.resolve(value) {
            PlaceClass::Restricted(u, subs) => {
                for s in subs {
                    self.ctx_expr(s);
                }
                let mut names = HashSet::new();
                collect_pattern_bindings(pattern, &mut names);
                for n in names {
                    self.lineage.insert(n, u.clone());
                }
            }
            PlaceClass::Poison => self.bad = true,
            PlaceClass::Free(subs) => {
                for s in subs {
                    self.ctx_expr(s);
                }
            }
            PlaceClass::NotLineage => self.ctx_expr(value),
        }
    }

    fn block(&mut self, b: &'e Block) {
        for s in &b.stmts {
            self.stmt(s);
        }
        if let Some(e) = &b.final_expr {
            self.ctx_expr(e);
        }
    }

    fn stmt(&mut self, s: &'e Stmt) {
        if self.bad {
            return;
        }
        match &s.kind {
            StmtKind::Let { pattern, value, .. } => match &pattern.kind {
                // 5c let-alias: a restricted RHS joins the lineage under the
                // bound name (PayloadScan never tracked these — the alias
                // could otherwise be handed bare to a mutating callee).
                PatternKind::Binding(name) => match self.resolve(value) {
                    PlaceClass::Restricted(u, subs) => {
                        for sub in subs {
                            self.ctx_expr(sub);
                        }
                        self.lineage.insert(name.clone(), u);
                    }
                    PlaceClass::Poison => self.bad = true,
                    PlaceClass::Free(subs) => {
                        for sub in subs {
                            self.ctx_expr(sub);
                        }
                    }
                    PlaceClass::NotLineage => self.ctx_expr(value),
                },
                _ => self.destructure(pattern, value),
            },
            StmtKind::LetElse {
                pattern,
                value,
                else_block,
                ..
            } => {
                self.destructure(pattern, value);
                self.block(else_block);
            }
            StmtKind::Expr(e) => self.ctx_expr(e),
            StmtKind::LetUninit { .. } => {}
            StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => self.block(body),
            StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
                // 5a: ANY projection-target store disqualifies — a field/index
                // store through any alias can release a count some elided
                // frame is relying on. Scalar-local assigns stay legal.
                if matches!(
                    &target.kind,
                    ExprKind::FieldAccess { .. }
                        | ExprKind::Index { .. }
                        | ExprKind::TupleIndex { .. }
                ) {
                    self.bad = true;
                    return;
                }
                self.ctx_expr(target);
                self.ctx_expr(value);
            }
            StmtKind::MultiAssign { targets, values } => {
                for t in targets {
                    if matches!(
                        &t.kind,
                        ExprKind::FieldAccess { .. }
                            | ExprKind::Index { .. }
                            | ExprKind::TupleIndex { .. }
                    ) {
                        self.bad = true;
                        return;
                    }
                    self.ctx_expr(t);
                }
                for v in values {
                    self.ctx_expr(v);
                }
            }
        }
    }
}

/// Fn-level condition-5 check against the CURRENT candidate set.
fn condition5_ok(
    func: &Function,
    recs: &[(String, usize)],
    elided: &HashMap<String, Vec<(String, usize)>>,
    carriers: &HashSet<String>,
    fields: &HashMap<(String, String), Option<String>>,
) -> bool {
    // 5b — every shared-carrying param must itself be elided: an owned (or
    // otherwise non-elided) sibling handle could alias the borrowed graph and
    // its perfectly-balanced mutations would still release borrowed nodes.
    let elided_names: HashSet<&str> = recs.iter().map(|(n, _)| n.as_str()).collect();
    let mut lineage = HashMap::new();
    for p in &func.params {
        let Some(name) = binding_name(&p.pattern) else {
            // Exotic param pattern — cannot track its lineage; fail closed.
            return false;
        };
        match type_shared_payload(&p.ty, carriers) {
            Some(u) => {
                if !elided_names.contains(name) {
                    return false;
                }
                lineage.insert(name.to_string(), u);
            }
            None => {
                // Handle-free param (scalar, Vec[i64], …) — no lineage.
            }
        }
    }
    let mut scan = Cond5 {
        elided,
        fields,
        lineage,
        bad: false,
        in_closure: false,
    };
    scan.block(&func.body);
    !scan.bad
}

fn binding_name(pat: &Pattern) -> Option<&str> {
    match &pat.kind {
        PatternKind::Binding(n) => Some(n.as_str()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run the analysis over `src` with hand-supplied param modes (decoupling the
    /// test from the ownership pass). Returns fn-name → elided (param, position).
    fn elidable(
        src: &str,
        modes: &[(&str, &[(&str, OwnershipMode)])],
    ) -> HashMap<String, Vec<(String, usize)>> {
        let parsed = crate::parse(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let pm: HashMap<String, Vec<(String, OwnershipMode)>> = modes
            .iter()
            .map(|(f, ps)| {
                (
                    f.to_string(),
                    ps.iter().map(|(n, m)| (n.to_string(), m.clone())).collect(),
                )
            })
            .collect();
        safe_elidable_ref_params(&parsed.program, &pm)
    }

    const NODE: &str =
        "shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }\n";

    /// Condition 4 keeps the #101 walkers: payloads used ONLY via field
    /// projections (`an.left`, `n.right`) into `ref` positions — never moved out.
    #[test]
    fn guard_keeps_projection_only_walk() {
        let src = format!(
            "{NODE}\
fn is_mirror(a: Option[Node], b: Option[Node]) -> bool {{ match a {{ None => match b {{ None => true, Some(_) => false }}, Some(an) => match b {{ None => false, Some(bn) => an.val == bn.val and is_mirror(an.left, bn.right) and is_mirror(an.right, bn.left) }} }} }}
fn is_symmetric(root: Option[Node]) -> bool {{ match root {{ None => true, Some(n) => is_mirror(n.left, n.right) }} }}
fn caller(pool: Vec[Option[Node]]) -> bool {{ is_symmetric(pool[0i64]) }}
"
        );
        let out = elidable(
            &src,
            &[
                (
                    "is_mirror",
                    &[("a", OwnershipMode::Ref), ("b", OwnershipMode::Ref)],
                ),
                ("is_symmetric", &[("root", OwnershipMode::Ref)]),
            ],
        );
        assert_eq!(
            out.get("is_mirror"),
            Some(&vec![("a".to_string(), 0), ("b".to_string(), 1)]),
            "is_mirror's projection-only params must stay elidable"
        );
        assert_eq!(
            out.get("is_symmetric"),
            Some(&vec![("root".to_string(), 0)]),
            "is_symmetric's projection-only param must stay elidable"
        );
    }

    /// Borrow-forward relaxation (#110): a thin wrapper `is_balanced(root)` that
    /// forwards its **`Ref`** param `root` by bare identifier to the recursive
    /// helper `check` is a *borrow*, not a move (a `Ref` param's referent is kept
    /// alive by the caller for the whole call), so `check`'s `node` stays
    /// elidable despite the bare-identifier call site.
    #[test]
    fn borrow_forward_of_ref_param_keeps_helper_elidable() {
        let src = format!(
            "{NODE}\
fn check(node: Option[Node]) -> i64 {{ match node {{ None => 0i64, Some(n) => {{ let l = check(n.left); let r = check(n.right); if l > r {{ 1i64 + l }} else {{ 1i64 + r }} }} }} }}
fn is_balanced(root: Option[Node]) -> bool {{ check(root) != -1i64 }}
fn caller(pool: Vec[Option[Node]]) -> bool {{ is_balanced(pool[0i64]) }}
"
        );
        let out = elidable(
            &src,
            &[
                ("check", &[("node", OwnershipMode::Ref)]),
                ("is_balanced", &[("root", OwnershipMode::Ref)]),
            ],
        );
        assert_eq!(
            out.get("check"),
            Some(&vec![("node".to_string(), 0)]),
            "check's node must stay elidable — is_balanced forwards a Ref param (a borrow), not a move"
        );
    }

    /// Negative control: a bare **local** (owned) binding passed by identifier is
    /// a genuine MOVE, not a borrow-forward — the helper must NOT elide (its exit
    /// dec is load-bearing). Only `Ref` *params* qualify for the relaxation.
    #[test]
    fn bare_local_forward_still_excludes_helper() {
        let src = format!(
            "{NODE}\
fn walk(node: Option[Node]) -> i64 {{ match node {{ None => 0i64, Some(n) => walk(n.left) + walk(n.right) }} }}
fn driver() -> i64 {{ let local = Some(Node {{ val: 1i64, left: None, right: None }}); walk(local) }}
"
        );
        let out = elidable(&src, &[("walk", &[("node", OwnershipMode::Ref)])]);
        assert_eq!(
            out.get("walk"),
            None,
            "walk must NOT elide — walk(local) forwards an owned local, a move, not a borrow"
        );
    }

    /// Condition 4 rejects a payload moved by value into a consuming callee —
    /// `match p { Some(n) => sink(n) }` — even though conditions 1–3 all hold.
    #[test]
    fn guard_rejects_bare_payload_consume() {
        let src = format!(
            "{NODE}\
fn sink(x: Node) -> i64 {{ x.val }}
fn probe(p: Option[Node]) -> i64 {{ match p {{ None => 0i64, Some(n) => sink(n) }} }}
fn caller(pool: Vec[Option[Node]]) -> i64 {{ probe(pool[0i64]) }}
"
        );
        let out = elidable(&src, &[("probe", &[("p", OwnershipMode::Ref)])]);
        assert!(
            !out.contains_key("probe"),
            "probe moves its payload into a consuming call — must NOT elide, got {out:?}"
        );
    }

    /// Forwarding a payload through a second call is still a bare move.
    #[test]
    fn guard_rejects_forwarded_payload() {
        let src = format!(
            "{NODE}\
fn sink(x: Node) -> i64 {{ x.val }}
fn forward(y: Node) -> i64 {{ sink(y) }}
fn probe2(p: Option[Node]) -> i64 {{ match p {{ None => 0i64, Some(n) => forward(n) }} }}
fn caller(pool: Vec[Option[Node]]) -> i64 {{ probe2(pool[0i64]) }}
"
        );
        let out = elidable(&src, &[("probe2", &[("p", OwnershipMode::Ref)])]);
        assert!(
            !out.contains_key("probe2"),
            "forwarded payload, got {out:?}"
        );
    }

    /// The if-let sugar route is closed too.
    #[test]
    fn guard_rejects_if_let_payload_consume() {
        let src = format!(
            "{NODE}\
fn sink(x: Node) -> i64 {{ x.val }}
fn probe3(p: Option[Node]) -> i64 {{ let mut r = 0i64; if let Some(n) = p {{ r = sink(n); }} r }}
fn caller(pool: Vec[Option[Node]]) -> i64 {{ probe3(pool[0i64]) }}
"
        );
        let out = elidable(&src, &[("probe3", &[("p", OwnershipMode::Ref)])]);
        assert!(
            !out.contains_key("probe3"),
            "if-let payload move, got {out:?}"
        );
    }

    /// An `@`-binding that aliases the whole scrutinee is a bare move of the
    /// referent, not a projection — rejected.
    #[test]
    fn guard_rejects_at_binding_alias() {
        let src = format!(
            "{NODE}\
fn hold(x: Option[Node]) -> i64 {{ match x {{ None => 0i64, Some(_) => 1i64 }} }}
fn probe4(p: Option[Node]) -> i64 {{ match p {{ whole @ Some(_) => hold(whole), None => 0i64 }} }}
fn caller(pool: Vec[Option[Node]]) -> i64 {{ probe4(pool[0i64]) }}
"
        );
        let out = elidable(&src, &[("probe4", &[("p", OwnershipMode::Ref)])]);
        assert!(
            !out.contains_key("probe4"),
            "@-binding aliases the referent — must NOT elide, got {out:?}"
        );
    }

    // ── Condition 5 (B-2026-07-16-7): read-only elided web ─────────────

    const BACKNODE: &str =
        "shared struct Node { val: i64, mut left: Option[Node], mut back: Option[Node] }\n";

    /// The reproduced miscompile: an elidable walk passes a lineage
    /// projection (`n.back`) to a callee that field-assigns through it,
    /// releasing the only count keeping the borrowed node alive. 5c must
    /// drop `walk` (the callee `detach` is dropped by 5a's store ban, so
    /// the projection flows to a non-elided position).
    #[test]
    fn cond5_rejects_projection_to_mutating_callee() {
        let src = format!(
            "{BACKNODE}\
fn detach(q: Option[Node]) -> i64 {{ match q {{ None => 0, Some(m) => {{ m.left = None; 1 }} }} }}
fn walk(p: Option[Node]) -> i64 {{ match p {{ None => 0, Some(n) => detach(n.back) + n.val }} }}
fn caller(root: Node) -> i64 {{ walk(root.left) }}
"
        );
        let out = elidable(
            &src,
            &[
                ("detach", &[("q", OwnershipMode::Ref)]),
                ("walk", &[("p", OwnershipMode::Ref)]),
            ],
        );
        assert!(
            !out.contains_key("walk") && !out.contains_key("detach"),
            "mutating-callee web must NOT elide, got {out:?}"
        );
    }

    /// 5a: a projection-target store inside an otherwise-elidable fn.
    #[test]
    fn cond5_rejects_field_store_in_elidable_fn() {
        let src = format!(
            "{BACKNODE}\
fn zap(p: Option[Node]) -> i64 {{ match p {{ None => 0, Some(n) => {{ n.left = None; n.val }} }} }}
fn caller(root: Node) -> i64 {{ zap(root.left) }}
"
        );
        let out = elidable(&src, &[("zap", &[("p", OwnershipMode::Ref)])]);
        assert!(
            !out.contains_key("zap"),
            "field-storing fn must NOT elide, got {out:?}"
        );
    }

    /// 5b: an owned shared sibling param could alias the borrowed graph.
    #[test]
    fn cond5_rejects_owned_shared_sibling_param() {
        let src = format!(
            "{BACKNODE}\
fn mixed(p: Option[Node], q: Node) -> i64 {{ match p {{ None => q.val, Some(n) => n.val }} }}
fn caller(root: Node, other: Node) -> i64 {{ mixed(root.left, other) }}
"
        );
        let out = elidable(
            &src,
            &[(
                "mixed",
                &[("p", OwnershipMode::Ref), ("q", OwnershipMode::Own)],
            )],
        );
        assert!(
            !out.contains_key("mixed"),
            "non-elided shared sibling param must drop the fn, got {out:?}"
        );
    }

    /// 5c let-alias: `let x = n.back` joins the lineage, so handing `x` to an
    /// owned-consuming callee is a bare lineage move — dropped.
    #[test]
    fn cond5_rejects_let_alias_flow_to_owned_callee() {
        let src = format!(
            "{BACKNODE}\
fn sink(x: Option[Node]) -> i64 {{ match x {{ None => 0, Some(m) => m.val }} }}
fn leaky(p: Option[Node]) -> i64 {{ match p {{ None => 0, Some(n) => {{ let x = n.back; sink(x) }} }} }}
fn caller(root: Node) -> i64 {{ leaky(root.left) }}
"
        );
        let out = elidable(
            &src,
            &[
                ("sink", &[("x", OwnershipMode::Ref)]),
                ("leaky", &[("p", OwnershipMode::Ref)]),
            ],
        );
        assert!(
            !out.contains_key("leaky"),
            "let-alias of a lineage projection handed to a callee bare must NOT elide the caller, got {out:?}"
        );
    }

    /// Fixed-point cascade: G is dropped (5a store), so F's projection-arg
    /// into G becomes non-sanctioned and F drops on the next iteration.
    #[test]
    fn cond5_cascade_removes_caller_of_dropped_callee() {
        let src = format!(
            "{BACKNODE}\
fn g(q: Option[Node]) -> i64 {{ match q {{ None => 0, Some(m) => {{ m.left = None; m.val }} }} }}
fn f(p: Option[Node]) -> i64 {{ match p {{ None => 0, Some(n) => g(n.left) + n.val }} }}
fn caller(root: Node) -> i64 {{ f(root.left) }}
"
        );
        let out = elidable(
            &src,
            &[
                ("g", &[("q", OwnershipMode::Ref)]),
                ("f", &[("p", OwnershipMode::Ref)]),
            ],
        );
        assert!(
            !out.contains_key("g") && !out.contains_key("f"),
            "cascade must remove both, got {out:?}"
        );
    }

    /// The known winners survive condition 5: recursion into the elided web
    /// itself, scalar field reads anywhere, nested destructures.
    #[test]
    fn cond5_keeps_recursive_readonly_walks() {
        let src = format!(
            "{NODE}\
fn sum(node: Option[Node]) -> i64 {{ match node {{ None => 0, Some(n) => n.val + sum(n.left) + sum(n.right) }} }}
fn caller(root: Node) -> i64 {{ sum(root.left) }}
"
        );
        let out = elidable(&src, &[("sum", &[("node", OwnershipMode::Ref)])]);
        assert!(
            out.get("sum")
                .is_some_and(|r| r.iter().any(|(n, p)| n == "node" && *p == 0)),
            "pure combine-both walk must stay elidable, got {out:?}"
        );
    }
}
