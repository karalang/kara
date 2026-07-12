//! RC-elision safety analysis (env `KARAC_RC_ELIDE_REF_PARAMS`).
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
//! Two independent conditions must both hold, or the balanced pair is NOT a
//! no-op and eliding it leaks (or double-frees):
//!
//! 1. **Caller side — the arg is a borrow, and its referent outlives the call.**
//!    Every call must pass the param a *projection of a named binding* —
//!    `n.left`, `v[i]`, `t.0`, `self.head` — which READS a sub-value out of a
//!    container the caller still holds (a genuine borrow; the container keeps it
//!    alive and drops it at the caller's own scope exit). A **bare** identifier
//!    (`eat(d)`) is rejected: passing a whole binding by value is a *move* that
//!    transfers its `+1` to the callee, whose exit dec is then load-bearing. A
//!    *fresh rvalue* (`Some(x)`, a call return) is rejected for the same reason.
//!    The function must also be **directly called** at least once (arg shapes
//!    fully observed), never used as a **value** (indirect calls invisible), and
//!    not **`pub`** (external callers invisible).
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
//! The analysis is deliberately **conservative and fail-closed**: the caller
//! scan is an exhaustive `match` with no `_` arm (a new AST node breaks the
//! build rather than silently admitting an escape), and the callee check treats
//! every param use other than a scrutinee as escaping. The worst case is a
//! missed optimization, never a leak. Codegen consumes the result via
//! `borrowed_arg_skip` / `borrowed_param_dec_skip`.

use crate::ast::{
    Block, Expr, ExprKind, Function, ImplItem, Item, ParsedInterpolationPart, Program, Stmt,
    StmtKind, TraitItem, TypeExpr, TypeKind,
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
                            if !is_borrow_projection(&arg.value) {
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
            other => {
                let mut recur = |e: &Expr| self.walk_expr(e);
                walk_children(other, &mut recur);
            }
        }
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
                if let ParsedInterpolationPart::Expr(e) = p {
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
// Driver
// ────────────────────────────────────────────────────────────────────────

/// Compute the `ref` parameters that are sound to RC-elide (see module docs):
/// function name → `(param_name, position)`.
pub fn safe_elidable_ref_params(
    program: &Program,
    param_modes: &HashMap<String, Vec<(String, OwnershipMode)>>,
) -> HashMap<String, Vec<(String, usize)>> {
    let fn_names: HashSet<&str> = param_modes.keys().map(|s| s.as_str()).collect();

    // Condition 1 — caller-side scan over EVERY function/method body.
    let mut scan = Scan {
        fn_names: &fn_names,
        called: HashSet::new(),
        unsafe_pos: HashMap::new(),
        escaped: HashSet::new(),
    };
    // Free functions the ownership pass knows about, by name (only these are
    // directly-called elision candidates; methods are excluded via `called`).
    let mut free_fns: HashMap<&str, &Function> = HashMap::new();
    for item in &program.items {
        match item {
            Item::Function(f) => {
                scan.walk_block(&f.body);
                free_fns.insert(f.name.as_str(), f);
            }
            Item::ImplBlock(b) => {
                for inner in &b.items {
                    if let ImplItem::Method(m) = inner {
                        scan.walk_block(&m.body);
                    }
                }
            }
            Item::TraitDef(t) => {
                for inner in &t.items {
                    if let TraitItem::Method(m) = inner {
                        if let Some(body) = &m.body {
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
            })
            .map(|(i, (n, _))| (n.clone(), i))
            .collect();
        if !recs.is_empty() {
            out.insert(fname.clone(), recs);
        }
    }

    if std::env::var_os("KARAC_RC_ELIDE_DEBUG").is_some() {
        eprintln!("[rc-elide] elidable set: {out:?}");
    }
    out
}
