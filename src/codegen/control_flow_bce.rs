//! Bounds-check elision (BCE) plumbing for the codegen pass.
//!
//! Recognises a conservative subset of guard expressions on if /
//! while / for-range heads — signed comparisons against identifiers
//! or zero, `and`-chained — and feeds the index-safety facts to the
//! per-`indexing` codegen so the runtime bounds check can be omitted
//! when the surrounding control flow already proves the index is in
//! range. Unrecognized shapes are silently ignored (the bounds check
//! stays as-is for the corresponding index).
//!
//! Houses `collect_asserted_bounds_from_guard` (if/while head
//! analysis), `collect_asserted_bounds_from_for_range` (for-range
//! analysis), `walk_guard_conjuncts` (the recursive
//! `and`-conjunct walker that gathers each leaf comparison), the
//! per-leaf classifier `extract_index_bound_from_binop`, and the
//! `len`-origin lookup `resolve_len_origin`.
//!
//! Lives in a sibling `impl<'ctx> super::Codegen<'ctx>` block.

use crate::ast::*;

use super::state::{AssertedIndexBound, MonotoneDir, VarSlot};

impl<'ctx> super::Codegen<'ctx> {
    /// Walk a boolean expression that holds true at the entry to a body
    /// block (e.g. a `while` guard or an `if` cond) and return the
    /// index-safety facts it asserts. Only handles `and`-chained signed
    /// comparisons against identifiers or zero — the conservative subset
    /// that the kata-5 elision pass needs. Unrecognized shapes are silently
    /// ignored (the bounds check stays as-is for the corresponding index).
    pub(super) fn collect_asserted_bounds_from_guard(
        &self,
        cond: &Expr,
    ) -> Vec<AssertedIndexBound> {
        let mut out = Vec::new();
        self.walk_guard_conjuncts(cond, &mut out);
        out
    }

    /// Asserted-bounds facts for the body of `for i in start..end`. The
    /// for-range loop natively establishes `start <= i < end` (or `<= end`
    /// for inclusive), so we can short-cut the guard-parsing surface for
    /// the common `for i in 0..v.len()` and `for i in 1..n` shapes.
    ///
    /// Lower bound: pushed when `start` is None (defaults to 0) or a
    /// non-negative integer literal. Anything else (a variable, an
    /// arithmetic expression) is conservative — we don't know its sign
    /// without range analysis, so no LowerBound fact.
    ///
    /// Upper bound: pushed only for exclusive ranges (`0..end`, not
    /// `0..=end`) when `end` resolves to a Vec or Slice's `.len()` via
    /// `resolve_len_origin`. Inclusive ranges include the end value
    /// itself, which would be one past the last valid index — proving
    /// `i < v.len()` inside the body would require knowing `end <
    /// v.len()`, which the source rarely makes explicit.
    pub(super) fn collect_asserted_bounds_from_for_range(
        &self,
        pattern: &Pattern,
        start: &Option<Box<Expr>>,
        end: &Option<Box<Expr>>,
        inclusive: bool,
    ) -> Vec<AssertedIndexBound> {
        let idx_var = match &pattern.kind {
            PatternKind::Binding(name) => name.clone(),
            _ => return Vec::new(),
        };
        let mut out = Vec::new();
        let lower_proven = match start.as_deref().map(|e| &e.kind) {
            None => true,
            Some(ExprKind::Integer(n, _)) if *n >= 0 => true,
            _ => false,
        };
        if lower_proven {
            out.push(AssertedIndexBound::LowerBound {
                idx_var: idx_var.clone(),
            });
        }
        if !inclusive {
            if let Some(e) = end.as_deref() {
                if let Some(vec_var) = self.resolve_len_origin(e) {
                    out.push(AssertedIndexBound::UpperBound { idx_var, vec_var });
                }
            }
        }
        out
    }

    pub(super) fn walk_guard_conjuncts(&self, cond: &Expr, out: &mut Vec<AssertedIndexBound>) {
        if let ExprKind::Binary { op, left, right } = &cond.kind {
            // Recurse through `and`-chained conjuncts so multi-clause
            // guards like `lo >= 0 and hi < n and chars[lo] == chars[hi]`
            // contribute each conjunct's fact independently.
            if matches!(op, BinOp::And) {
                self.walk_guard_conjuncts(left, out);
                self.walk_guard_conjuncts(right, out);
                return;
            }
            if let Some(fact) = self.extract_index_bound_from_binop(op, left, right) {
                out.push(fact);
            }
        }
        // The typechecker rewrites integer comparisons through trait-method
        // dispatch (e.g. `lo >= 0` → `i64::ge(lo, 0)`), so the post-lowering
        // AST carries `>=` / `<=` / sometimes `<` / `>` as `Call` nodes whose
        // callee is a `Path { segments: ["<int>", "ge"|"le"|"lt"|"gt"], .. }`.
        // The Binary form above still handles the cases the lowering leaves
        // alone (which empirically includes `<` between two same-typed i64s);
        // this Call arm catches the rest.
        if let ExprKind::Call { callee, args } = &cond.kind {
            if let ExprKind::Path { segments, .. } = &callee.kind {
                if segments.len() == 2 && args.len() == 2 {
                    let op = match segments[1].as_str() {
                        "ge" => Some(BinOp::GtEq),
                        "le" => Some(BinOp::LtEq),
                        "lt" => Some(BinOp::Lt),
                        "gt" => Some(BinOp::Gt),
                        _ => None,
                    };
                    if let Some(op) = op {
                        if let Some(fact) =
                            self.extract_index_bound_from_binop(&op, &args[0].value, &args[1].value)
                        {
                            out.push(fact);
                        }
                    }
                }
            }
        }
    }

    /// Match a single binary comparison and decode whichever index-safety
    /// fact (if any) it establishes. Recognizes the four normal forms
    /// the kata's `while`-guard surface produces:
    ///   - `idx >= 0`  /  `0 <= idx`           → LowerBound { idx }
    ///   - `idx < vec.len()`                    → UpperBound { idx, vec }
    ///   - `idx < n` where n aliases vec.len()  → UpperBound { idx, vec }
    ///
    /// Strict-less only — `idx <= n-1` would be sound but isn't a shape
    /// the kata surface produces, and conservatively skipping it now keeps
    /// the elision predicate small.
    pub(super) fn extract_index_bound_from_binop(
        &self,
        op: &BinOp,
        left: &Expr,
        right: &Expr,
    ) -> Option<AssertedIndexBound> {
        match op {
            // `idx >= 0`
            BinOp::GtEq => {
                if let (ExprKind::Identifier(idx), ExprKind::Integer(0, _)) =
                    (&left.kind, &right.kind)
                {
                    return Some(AssertedIndexBound::LowerBound {
                        idx_var: idx.clone(),
                    });
                }
                None
            }
            // `0 <= idx`
            BinOp::LtEq => {
                if let (ExprKind::Integer(0, _), ExprKind::Identifier(idx)) =
                    (&left.kind, &right.kind)
                {
                    return Some(AssertedIndexBound::LowerBound {
                        idx_var: idx.clone(),
                    });
                }
                None
            }
            // `idx < n` where n is either `vec.len()` (resolved here) or a
            // local binding to one (resolved via `len_alias`).
            BinOp::Lt => {
                if let ExprKind::Identifier(idx) = &left.kind {
                    let vec_var = self.resolve_len_origin(right)?;
                    return Some(AssertedIndexBound::UpperBound {
                        idx_var: idx.clone(),
                        vec_var,
                    });
                }
                None
            }
            // `n > idx` — same fact as `idx < n`.
            BinOp::Gt => {
                if let ExprKind::Identifier(idx) = &right.kind {
                    let vec_var = self.resolve_len_origin(left)?;
                    return Some(AssertedIndexBound::UpperBound {
                        idx_var: idx.clone(),
                        vec_var,
                    });
                }
                None
            }
            _ => None,
        }
    }

    /// Resolve an expression to the Vec / Slice variable whose `.len()`
    /// it computes, if any. Handles:
    ///   - Direct `coll.len()` method call (Identifier receiver, either
    ///     a Vec or a Slice).
    ///   - A bare Identifier whose binding was previously recorded in
    ///     `len_alias` by the let-site tracking pass (which also covers
    ///     both Vec and Slice receivers).
    pub(super) fn resolve_len_origin(&self, expr: &Expr) -> Option<String> {
        match &expr.kind {
            ExprKind::MethodCall {
                object,
                method,
                args,
                ..
            } if method == "len" && args.is_empty() => {
                if let ExprKind::Identifier(coll_name) = &object.kind {
                    if self.vec_elem_types.contains_key(coll_name.as_str())
                        || self.slice_elem_types.contains_key(coll_name.as_str())
                    {
                        return Some(coll_name.clone());
                    }
                }
                None
            }
            // A local binding to `v.len()` (`let n = v.len()`) resolves the guard
            // RHS back to the Vec.
            ExprKind::Identifier(name) => {
                if let Some(v) = self.len_alias.get(name.as_str()) {
                    return Some(v.clone());
                }
                self.resolve_len_pin(expr)
            }
            // A length pin (`v` filled to exactly `bound` elements by a counted
            // loop — see bce_length_pin.rs) can match an arithmetic bound like
            // `cols + 1`, not just a bare identifier.
            _ => self.resolve_len_pin(expr),
        }
    }

    /// Match `expr` against the active length pins by normalising it to a
    /// span-free `BoundTerm` and comparing structurally. Returns the pinned Vec
    /// whose length equals that bound, if any.
    fn resolve_len_pin(&self, expr: &Expr) -> Option<String> {
        let bt = super::bce_length_pin::normalize_bound(expr)?;
        self.vec_len_pins
            .iter()
            .find(|(bound, _)| *bound == bt)
            .map(|(_, vec)| vec.clone())
    }
}

// ── Monotone loop variables → `llvm.assume` range facts ─────────
//
// The split-check elision above handles bounds the source GUARD proves.
// This section handles the complementary class the 2026-06-07 diagnostic
// pinned (docs/investigations/bce_monotonic_assume.md): conditionally-
// updated monotone variables (compaction write heads, merge cursors),
// whose phis are not AddRecs — SCEV and LVI both give up on them, so
// LLVM keeps the bounds check even when its post-inline constant
// knowledge could complete the proof. karac supplies the half it can
// prove syntactically — "x never moves above/below its loop-entry
// value" — as an `llvm.assume` at body entry; LLVM combines it with
// the facts only it can see (inlined parameter constants, dominating
// guards) and folds the check. The assume is consumed by the optimizer
// (zero residue measured on the kata-26/88 shapes).
//
// SOUNDNESS. `assume(x <= init)` for a decreasing variable holds only
// if the update cannot wrap below MIN (a wrap would produce a huge
// positive value, violating the assume → UB injection). This is
// guaranteed by AOT integer-overflow trapping (landed 2026-06-07,
// same-day prerequisite — see phase-7-codegen.md § AOT integer-overflow
// trapping): a wrapping `x = x - c` panics before the wrapped value
// exists. The scan itself is fail-closed three ways:
//   1. The stmt/expr walks match `StmtKind`/`ExprKind` EXHAUSTIVELY (no
//      wildcard arm), so a new AST variant breaks this file at compile
//      time instead of silently escaping the write analysis.
//   2. Any write shape other than the recognized `x = x ± <non-negative
//      int literal>` forms — including plain reassignment, mut-marked
//      call args, method calls on `x` (mut-ref receivers), `ptr.*`
//      aliasing, and shadowing rebinds (`let x`, pattern bindings,
//      closure params) — poisons the name.
//   3. Emission covers every non-poisoned monotone counter, not only
//      those used as an array index. The fact `x >= / <= init` is sound
//      for ANY monotone counter (soundness rests on overflow trapping,
//      above) and LLVM discards it where useless (zero residue), so
//      there is no reason to gate on a single consumer. Array-index
//      counters feed bounds-check elision; arithmetic counters feed
//      overflow-check elision — e.g. a `while c < cols` counter in an
//      `(i*7 + c*3 + k) % 13` obstacle predicate whose non-negativity
//      lets LLVM prove the sum can't overflow and drop the checks
//      (kata #63, ledger B-2026-07-08-3). Restricting to index use left
//      the arithmetic consumers unserved.
// Updates inside nested loops/branches/closures stay eligible —
// monotonicity cares about direction, not update count per iteration.

#[derive(Default)]
struct MonotoneScan {
    /// Names with at least one recognized monotone update, and the
    /// (so-far) consistent direction. Inconsistent directions poison.
    dirs: std::collections::HashMap<String, MonotoneDir>,
    /// Names disqualified by any non-monotone write/alias/shadow.
    poisoned: std::collections::HashSet<String>,
}

impl MonotoneScan {
    fn poison(&mut self, name: &str) {
        self.poisoned.insert(name.to_string());
    }

    fn record(&mut self, name: &str, dir: MonotoneDir) {
        match self.dirs.get(name) {
            None => {
                self.dirs.insert(name.to_string(), dir);
            }
            Some(d) if *d == dir => {}
            Some(_) => self.poison(name),
        }
    }
}

/// `x = x + c` / `c + x` / `x - c` with `c` a non-negative integer
/// literal — in both the surface `Binary` form and the trait-method-
/// lowered `Call { Path([ty, "add"|"sub"]), [x, c] }` form (mirror of
/// `walk_guard_conjuncts`' comparison handling). Anything else: None.
fn classify_monotone_rhs(x: &str, value: &Expr) -> Option<MonotoneDir> {
    match &value.kind {
        ExprKind::Binary { op, left, right } => match op {
            BinOp::Add => match (&left.kind, &right.kind) {
                (ExprKind::Identifier(l), ExprKind::Integer(c, _)) if l == x && *c >= 0 => {
                    Some(MonotoneDir::Increasing)
                }
                (ExprKind::Integer(c, _), ExprKind::Identifier(r)) if r == x && *c >= 0 => {
                    Some(MonotoneDir::Increasing)
                }
                _ => None,
            },
            BinOp::Sub => match (&left.kind, &right.kind) {
                (ExprKind::Identifier(l), ExprKind::Integer(c, _)) if l == x && *c >= 0 => {
                    Some(MonotoneDir::Decreasing)
                }
                _ => None,
            },
            _ => None,
        },
        ExprKind::Call { callee, args } => {
            let ExprKind::Path { segments, .. } = &callee.kind else {
                return None;
            };
            if segments.len() != 2 || args.len() != 2 {
                return None;
            }
            let dir = match segments[1].as_str() {
                "add" => MonotoneDir::Increasing,
                "sub" => MonotoneDir::Decreasing,
                _ => return None,
            };
            match (&args[0].value.kind, &args[1].value.kind) {
                (ExprKind::Identifier(l), ExprKind::Integer(c, _)) if l == x && *c >= 0 => {
                    Some(dir)
                }
                // `add` is commutative; `sub` is not.
                (ExprKind::Integer(c, _), ExprKind::Identifier(r))
                    if r == x && *c >= 0 && dir == MonotoneDir::Increasing =>
                {
                    Some(dir)
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// Root identifier of a place expression (`x`, `x.f`, `x[i]`, `x.0`,
/// `*x`, `x?`-chains). None for non-place shapes (literals, calls,
/// temporaries) — those have no aliasable local to poison.
fn place_root_ident(expr: &Expr) -> Option<&str> {
    match &expr.kind {
        ExprKind::Identifier(name) => Some(name.as_str()),
        ExprKind::FieldAccess { object, .. }
        | ExprKind::TupleIndex { object, .. }
        | ExprKind::Index { object, .. }
        | ExprKind::OptionalChain { object, .. } => place_root_ident(object),
        ExprKind::Unary { operand, .. } => place_root_ident(operand),
        ExprKind::Question(inner) => place_root_ident(inner),
        _ => None,
    }
}

fn mono_scan_block(b: &Block, s: &mut MonotoneScan) {
    for stmt in &b.stmts {
        mono_scan_stmt(stmt, s);
    }
    if let Some(e) = &b.final_expr {
        mono_scan_expr(e, s);
    }
}

fn mono_scan_stmt(stmt: &Stmt, s: &mut MonotoneScan) {
    match &stmt.kind {
        StmtKind::MultiAssign { .. } => unreachable!(
            "StmtKind::MultiAssign is removed by the desugar pass before reaching this phase"
        ),
        StmtKind::Let { pattern, value, .. } => {
            // A body-local `let x` shadows (or re-initializes) the name —
            // by-name tracking can no longer tell the bindings apart.
            for n in pattern.binding_names() {
                s.poison(&n);
            }
            mono_scan_expr(value, s);
        }
        StmtKind::LetUninit { name, .. } => s.poison(name),
        StmtKind::LetElse {
            pattern,
            value,
            else_block,
            ..
        } => {
            for n in pattern.binding_names() {
                s.poison(&n);
            }
            mono_scan_expr(value, s);
            mono_scan_block(else_block, s);
        }
        StmtKind::Defer { body } => mono_scan_block(body, s),
        StmtKind::ErrDefer { binding, body } => {
            if let Some(b) = binding {
                s.poison(b);
            }
            mono_scan_block(body, s);
        }
        StmtKind::Assign { target, value } => {
            if let ExprKind::Identifier(x) = &target.kind {
                match classify_monotone_rhs(x, value) {
                    Some(dir) => s.record(x, dir),
                    None => s.poison(x),
                }
            } else {
                // Field / index / deref store — the root local's contents
                // change in a shape we don't model.
                if let Some(root) = place_root_ident(target) {
                    s.poison(root);
                }
                mono_scan_expr(target, s);
            }
            mono_scan_expr(value, s);
        }
        StmtKind::CompoundAssign { target, op, value } => {
            if let ExprKind::Identifier(x) = &target.kind {
                let dir = match (op, &value.kind) {
                    (CompoundOp::Add, ExprKind::Integer(c, _)) if *c >= 0 => {
                        Some(MonotoneDir::Increasing)
                    }
                    (CompoundOp::Sub, ExprKind::Integer(c, _)) if *c >= 0 => {
                        Some(MonotoneDir::Decreasing)
                    }
                    _ => None,
                };
                match dir {
                    Some(d) => s.record(x, d),
                    None => s.poison(x),
                }
            } else {
                if let Some(root) = place_root_ident(target) {
                    s.poison(root);
                }
                mono_scan_expr(target, s);
            }
            mono_scan_expr(value, s);
        }
        StmtKind::Expr(e) => mono_scan_expr(e, s),
    }
}

fn mono_scan_call_args(args: &[CallArg], s: &mut MonotoneScan) {
    for a in args {
        if a.mut_marker {
            // `f(mut x)` — the callee takes `mut ref` and may write x.
            if let Some(root) = place_root_ident(&a.value) {
                s.poison(root);
            }
        }
        mono_scan_expr(&a.value, s);
    }
}

/// EXHAUSTIVE over `ExprKind` — no wildcard arm, by design (see the
/// module-section comment's fail-closed rationale).
fn mono_scan_expr(e: &Expr, s: &mut MonotoneScan) {
    match &e.kind {
        ExprKind::Integer(_, _)
        | ExprKind::Float(_, _)
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
        | ExprKind::Error => {}
        ExprKind::InterpolatedStringLit(parts) => {
            for p in parts {
                if let ParsedInterpolationPart::Expr(inner) = p {
                    mono_scan_expr(inner, s);
                }
            }
        }
        ExprKind::Binary { left, right, .. } => {
            mono_scan_expr(left, s);
            mono_scan_expr(right, s);
        }
        ExprKind::Unary { operand, .. } => mono_scan_expr(operand, s),
        ExprKind::Question(inner) => mono_scan_expr(inner, s),
        ExprKind::OptionalChain { object, args, .. } => {
            // Method-call-like: the receiver may be taken `mut ref self`.
            if let Some(root) = place_root_ident(object) {
                s.poison(root);
            }
            mono_scan_expr(object, s);
            if let Some(a) = args {
                mono_scan_call_args(a, s);
            }
        }
        ExprKind::NilCoalesce { left, right } => {
            mono_scan_expr(left, s);
            mono_scan_expr(right, s);
        }
        ExprKind::Call { callee, args } => {
            // `ptr.*` builtins can take a local's address and write through
            // it later — poison every place root in the argument list.
            if let ExprKind::Path { segments, .. } = &callee.kind {
                if segments.first().is_some_and(|s0| s0 == "ptr") {
                    for a in args {
                        if let Some(root) = place_root_ident(&a.value) {
                            s.poison(root);
                        }
                    }
                }
            }
            mono_scan_expr(callee, s);
            mono_scan_call_args(args, s);
        }
        ExprKind::MethodCall { object, args, .. } => {
            // Methods may take `mut ref self` — poison the receiver root.
            if let Some(root) = place_root_ident(object) {
                s.poison(root);
            }
            mono_scan_expr(object, s);
            mono_scan_call_args(args, s);
        }
        ExprKind::FieldAccess { object, .. } => mono_scan_expr(object, s),
        ExprKind::TupleIndex { object, .. } => mono_scan_expr(object, s),
        ExprKind::Index { object, index } => {
            mono_scan_expr(object, s);
            mono_scan_expr(index, s);
        }
        ExprKind::Block(b) | ExprKind::Comptime(b) => mono_scan_block(b, s),
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            mono_scan_expr(condition, s);
            mono_scan_block(then_block, s);
            if let Some(e) = else_branch {
                mono_scan_expr(e, s);
            }
        }
        ExprKind::IfLet {
            pattern,
            value,
            then_block,
            else_branch,
        } => {
            for n in pattern.binding_names() {
                s.poison(&n);
            }
            mono_scan_expr(value, s);
            mono_scan_block(then_block, s);
            if let Some(e) = else_branch {
                mono_scan_expr(e, s);
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            mono_scan_expr(scrutinee, s);
            for arm in arms {
                for n in arm.pattern.binding_names() {
                    s.poison(&n);
                }
                if let Some(g) = &arm.guard {
                    mono_scan_expr(g, s);
                }
                mono_scan_expr(&arm.body, s);
            }
        }
        ExprKind::While {
            condition, body, ..
        } => {
            mono_scan_expr(condition, s);
            mono_scan_block(body, s);
        }
        ExprKind::WhileLet {
            pattern,
            value,
            body,
            ..
        } => {
            for n in pattern.binding_names() {
                s.poison(&n);
            }
            mono_scan_expr(value, s);
            mono_scan_block(body, s);
        }
        ExprKind::For {
            pattern,
            iterable,
            body,
            ..
        } => {
            for n in pattern.binding_names() {
                s.poison(&n);
            }
            mono_scan_expr(iterable, s);
            mono_scan_block(body, s);
        }
        ExprKind::Loop { body, .. } => mono_scan_block(body, s),
        ExprKind::LabeledBlock { body, .. } => mono_scan_block(body, s),
        ExprKind::Closure { params, body, .. } => {
            for cp in params {
                for n in cp.pattern.binding_names() {
                    s.poison(&n);
                }
            }
            mono_scan_expr(body, s);
        }
        ExprKind::Return(opt) => {
            if let Some(inner) = opt {
                mono_scan_expr(inner, s);
            }
        }
        ExprKind::Break { value, .. } => {
            if let Some(v) = value {
                mono_scan_expr(v, s);
            }
        }
        ExprKind::Tuple(exprs) | ExprKind::ArrayLiteral(exprs) => {
            for x in exprs {
                mono_scan_expr(x, s);
            }
        }
        ExprKind::PrefixCollectionLiteral { items, .. } => {
            for x in items {
                mono_scan_expr(x, s);
            }
        }
        ExprKind::RepeatLiteral { value, count, .. } => {
            mono_scan_expr(value, s);
            mono_scan_expr(count, s);
        }
        ExprKind::MapLiteral(pairs) => {
            for (k, v) in pairs {
                mono_scan_expr(k, s);
                mono_scan_expr(v, s);
            }
        }
        ExprKind::StructLiteral { fields, spread, .. } => {
            for f in fields {
                mono_scan_expr(&f.value, s);
            }
            if let Some(sp) = spread {
                mono_scan_expr(sp, s);
            }
        }
        ExprKind::Pipe { left, right } => {
            mono_scan_expr(left, s);
            mono_scan_expr(right, s);
        }
        ExprKind::Cast { expr, .. } => mono_scan_expr(expr, s),
        ExprKind::OffsetOf { .. } => {}
        ExprKind::Range { start, end, .. } => {
            if let Some(st) = start {
                mono_scan_expr(st, s);
            }
            if let Some(en) = end {
                mono_scan_expr(en, s);
            }
        }
        ExprKind::Unsafe(b) | ExprKind::Try(b) | ExprKind::Seq(b) | ExprKind::Par(b) => {
            mono_scan_block(b, s);
        }
        ExprKind::Lock { body, .. } => mono_scan_block(body, s),
        ExprKind::Providers { bindings, body } => {
            for pb in bindings {
                mono_scan_expr(&pb.value, s);
            }
            mono_scan_block(body, s);
        }
    }
}

/// One monotone variable prepared for assume emission: its slot, the
/// direction, and the loop-entry value loaded in the preheader.
pub(super) struct MonotoneInit<'ctx> {
    name: String,
    dir: MonotoneDir,
    slot: VarSlot<'ctx>,
    init: inkwell::values::IntValue<'ctx>,
}

impl<'ctx> super::Codegen<'ctx> {
    /// Scan a loop guard + body and return the qualifying monotone
    /// variables (every non-poisoned monotone counter, index or not),
    /// deterministically ordered. See the monotone-scan section comment
    /// above for the qualification rules.
    pub(super) fn collect_monotone_vars(
        &self,
        guard: Option<&Expr>,
        body: &Block,
    ) -> Vec<(String, MonotoneDir)> {
        let mut scan = MonotoneScan::default();
        if let Some(g) = guard {
            mono_scan_expr(g, &mut scan);
        }
        mono_scan_block(body, &mut scan);
        let MonotoneScan { dirs, poisoned } = scan;
        let mut out: Vec<(String, MonotoneDir)> = dirs
            .into_iter()
            .filter(|(name, _)| !poisoned.contains(name))
            .collect();
        // HashMap order is nondeterministic; sort so IR output is stable
        // across runs (build reproducibility + IR-test pinning).
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// Load each monotone variable's loop-entry value at the current
    /// builder position (the loop PREHEADER — must be called before the
    /// branch into the loop machinery). Skips names without an int-typed
    /// local slot (module bindings, non-int types).
    pub(super) fn load_monotone_inits(
        &self,
        vars: &[(String, MonotoneDir)],
    ) -> Vec<MonotoneInit<'ctx>> {
        let mut out = Vec::new();
        for (name, dir) in vars {
            let Some(slot) = self.variables.get(name).copied() else {
                continue;
            };
            if !slot.ty.is_int_type() {
                continue;
            }
            let init = self
                .builder
                .build_load(slot.ty, slot.ptr, &format!("{name}.mono.init"))
                .unwrap()
                .into_int_value();
            out.push(MonotoneInit {
                name: name.clone(),
                dir: *dir,
                slot,
                init,
            });
        }
        out
    }

    /// Emit `llvm.assume(x >= init)` (Increasing) / `(x <= init)`
    /// (Decreasing) at the current builder position — the loop BODY
    /// entry. The optimizer consumes the assumes (zero residue measured);
    /// soundness rests on AOT overflow trapping (see section comment).
    pub(super) fn emit_monotone_assumes(&self, inits: &[MonotoneInit<'ctx>]) {
        if inits.is_empty() {
            return;
        }
        let assume =
            inkwell::intrinsics::Intrinsic::find("llvm.assume").expect("llvm.assume must exist");
        // Not overloaded, so empty param-types is correct (mirror of
        // reduce.rs's existing llvm.assume emission).
        let assume_fn = assume
            .get_declaration(&self.module, &[])
            .expect("llvm.assume declaration");
        for mi in inits {
            let cur = self
                .builder
                .build_load(mi.slot.ty, mi.slot.ptr, &format!("{}.mono.cur", mi.name))
                .unwrap()
                .into_int_value();
            let pred = match mi.dir {
                MonotoneDir::Increasing => inkwell::IntPredicate::SGE,
                MonotoneDir::Decreasing => inkwell::IntPredicate::SLE,
            };
            let fact = self
                .builder
                .build_int_compare(pred, cur, mi.init, &format!("{}.mono.fact", mi.name))
                .unwrap();
            self.builder
                .build_call(assume_fn, &[fact.into()], "")
                .unwrap();
        }
    }
}

// ===================================================================
// Binary-search midpoint assumes
// ===================================================================
//
// The monotone-assume tier above folds bounds checks on conditionally-
// updated cursors. It does NOT reach the canonical binary search
//
//     while lo < hi { let mid = lo + (hi - lo) / 2; ... nums[mid] ... }
//
// because the surviving check is on the DERIVED `mid`, and folding
// `mid < len` needs the RELATIONAL invariant `mid < hi` (correlating
// `lo` and `hi` inside `mid`'s definition). LLVM's CVP/LVI is interval-
// based — it bounds `mid` componentwise as `[lo_min+div_min,
// lo_max+div_max]` and cannot derive `mid < hi`; the
// `mid = extractvalue(sadd.with.overflow …)` value is additionally
// opaque to its range pass (see `docs/investigations/
// bce_monotonic_assume.md`). Validated in `opt`: supplying the two
// relational facts `mid >= lo` and `mid < hi` as `llvm.assume`s folds
// the check (zero residue), and neither absolute monotone bound on
// `lo`/`hi` is needed once both relational facts are present.
//
// SOUNDNESS. Both facts are LOCALLY sound from the midpoint form plus
// the dominating strict guard `lo < hi` (so `d = hi - lo >= 1`), with
// no whole-loop monotonicity analysis:
//   * `(hi - lo) / 2` is signed floor division of `d >= 1`, landing in
//     `[0, d - 1]`. Hence `lo <= mid = lo + (hi-lo)/2 <= lo + d - 1 =
//     hi - 1 < hi`. The `(lo + hi) / 2` form lands in `[lo, hi)` for
//     `lo < hi` by the same floor-division bound.
//   * AOT integer-overflow trapping (design.md § Arithmetic Overflow)
//     makes any wrapping `hi - lo` / `lo + …` panic before the wrapped
//     value exists, so the facts hold on every DEFINED execution — the
//     same soundness gate the monotone tier rests on.
// The assumes are emitted at the binding site, where `lo`/`hi` still
// hold the values `mid` was derived from, so later mutation of `lo` /
// `hi` in the loop body cannot retroactively falsify them.

/// A bare `ExprKind::Identifier`'s name, else `None`. Deliberately
/// narrower than `place_root_ident` — only a simple variable can name a
/// guard/midpoint operand.
fn simple_ident(e: &Expr) -> Option<&str> {
    match &e.kind {
        ExprKind::Identifier(n) => Some(n.as_str()),
        _ => None,
    }
}

/// `e` as `(a, b)` when it is `a + b` — surface `Binary(Add)` or the
/// trait-lowered `Call { Path([ty, "add"]), [a, b] }`.
fn as_add(e: &Expr) -> Option<(&Expr, &Expr)> {
    as_binop(e, BinOp::Add, "add")
}

/// `e` as `(a, b)` when it is `a - b` (surface or trait-lowered).
fn as_sub(e: &Expr) -> Option<(&Expr, &Expr)> {
    as_binop(e, BinOp::Sub, "sub")
}

fn as_binop<'e>(e: &'e Expr, op: BinOp, method: &str) -> Option<(&'e Expr, &'e Expr)> {
    match &e.kind {
        ExprKind::Binary { op: o, left, right } if *o == op => Some((left, right)),
        ExprKind::Call { callee, args } => {
            let ExprKind::Path { segments, .. } = &callee.kind else {
                return None;
            };
            if segments.len() == 2 && segments[1] == method && args.len() == 2 {
                Some((&args[0].value, &args[1].value))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// The dividend `x` when `e` is `x / 2` (surface `Binary(Div)` by the
/// integer literal 2, or the trait-lowered `Call { Path([ty, "div"]),
/// [x, 2] }`).
fn as_div_by_2(e: &Expr) -> Option<&Expr> {
    let is_two = |x: &Expr| matches!(&x.kind, ExprKind::Integer(2, _));
    match &e.kind {
        ExprKind::Binary {
            op: BinOp::Div,
            left,
            right,
        } if is_two(right) => Some(left),
        ExprKind::Call { callee, args } => {
            let ExprKind::Path { segments, .. } = &callee.kind else {
                return None;
            };
            if segments.len() == 2
                && segments[1] == "div"
                && args.len() == 2
                && is_two(&args[1].value)
            {
                Some(&args[0].value)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// True iff `value` computes the midpoint of `lo`/`hi`: `lo + (hi-lo)/2`,
/// `(hi-lo)/2 + lo`, or `(lo+hi)/2` (commutative in the sum). Both yield
/// `mid in [lo, hi)` under `lo < hi` (see the section SOUNDNESS note).
fn expr_is_midpoint(value: &Expr, lo: &str, hi: &str) -> bool {
    // `(lo + hi) / 2` (or `(hi + lo) / 2`).
    if let Some(dividend) = as_div_by_2(value) {
        if let Some((a, b)) = as_add(dividend) {
            let a = simple_ident(a);
            let b = simple_ident(b);
            if (a == Some(lo) && b == Some(hi)) || (a == Some(hi) && b == Some(lo)) {
                return true;
            }
        }
    }
    // `lo + (hi - lo) / 2` (or the sum commuted).
    if let Some((a, b)) = as_add(value) {
        for (x, y) in [(a, b), (b, a)] {
            if simple_ident(x) == Some(lo) {
                if let Some(dividend) = as_div_by_2(y) {
                    if let Some((s1, s2)) = as_sub(dividend) {
                        if simple_ident(s1) == Some(hi) && simple_ident(s2) == Some(lo) {
                            return true;
                        }
                    }
                }
            }
        }
    }
    false
}

impl<'ctx> super::Codegen<'ctx> {
    /// `(lo, hi)` when `cond` is a strict `lo < hi` between two distinct
    /// bare identifiers — surface `Binary(Lt)` or trait-lowered
    /// `Call { Path([ty, "lt"]), [lo, hi] }`. The strict form is required:
    /// the midpoint upper fact `mid < hi` only holds for `lo < hi`
    /// (a `lo <= hi` guard admits `lo == hi`, where `mid == hi`).
    pub(super) fn binsearch_guard_pair(cond: &Expr) -> Option<(String, String)> {
        let (l, r) = match &cond.kind {
            ExprKind::Binary {
                op: BinOp::Lt,
                left,
                right,
            } => (simple_ident(left)?, simple_ident(right)?),
            ExprKind::Call { callee, args } => {
                let ExprKind::Path { segments, .. } = &callee.kind else {
                    return None;
                };
                if segments.len() != 2 || segments[1] != "lt" || args.len() != 2 {
                    return None;
                }
                (simple_ident(&args[0].value)?, simple_ident(&args[1].value)?)
            }
            _ => None?,
        };
        if l != r {
            Some((l.to_string(), r.to_string()))
        } else {
            None
        }
    }

    /// Emit `assume(mid >= lo)` + `assume(mid < hi)` when a `let mid = …`
    /// binding under a dominating strict `lo < hi` guard computes the
    /// midpoint of `lo`/`hi`. Folds the otherwise-surviving `nums[mid]`
    /// bounds check (see the section comment). No-op outside a binary-
    /// search guard, for non-midpoint RHS, or when the binding shadows a
    /// guard variable.
    pub(super) fn try_emit_binsearch_midpoint_assumes(&mut self, pattern: &Pattern, value: &Expr) {
        let Some((lo, hi)) = self.binsearch_guard_stack.last().cloned() else {
            return;
        };
        let PatternKind::Binding(mid) = &pattern.kind else {
            return;
        };
        // A binding that shadows a guard var repurposes that slot — the
        // loaded `lo`/`hi` below would no longer be the midpoint operands.
        if mid == &lo || mid == &hi {
            return;
        }
        if !expr_is_midpoint(value, &lo, &hi) {
            return;
        }
        let (Some(mid_slot), Some(lo_slot), Some(hi_slot)) = (
            self.variables.get(mid).copied(),
            self.variables.get(&lo).copied(),
            self.variables.get(&hi).copied(),
        ) else {
            return;
        };
        // All three must be same-width integer slots (the comparisons are
        // i64-on-i64; a width mismatch or non-int slot means this isn't the
        // primitive-index binary search we recognise — bail, keep the check).
        if !mid_slot.ty.is_int_type() || !lo_slot.ty.is_int_type() || !hi_slot.ty.is_int_type() {
            return;
        }
        let mid_w = mid_slot.ty.into_int_type().get_bit_width();
        if mid_w != lo_slot.ty.into_int_type().get_bit_width()
            || mid_w != hi_slot.ty.into_int_type().get_bit_width()
        {
            return;
        }

        let assume =
            inkwell::intrinsics::Intrinsic::find("llvm.assume").expect("llvm.assume must exist");
        let assume_fn = assume
            .get_declaration(&self.module, &[])
            .expect("llvm.assume declaration");
        let mid_v = self
            .builder
            .build_load(mid_slot.ty, mid_slot.ptr, "bs.mid")
            .unwrap()
            .into_int_value();
        let lo_v = self
            .builder
            .build_load(lo_slot.ty, lo_slot.ptr, "bs.lo")
            .unwrap()
            .into_int_value();
        let hi_v = self
            .builder
            .build_load(hi_slot.ty, hi_slot.ptr, "bs.hi")
            .unwrap()
            .into_int_value();
        let ge = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SGE, mid_v, lo_v, "bs.mid.ge.lo")
            .unwrap();
        self.builder
            .build_call(assume_fn, &[ge.into()], "")
            .unwrap();
        let lt = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SLT, mid_v, hi_v, "bs.mid.lt.hi")
            .unwrap();
        self.builder
            .build_call(assume_fn, &[lt.into()], "")
            .unwrap();
        // Flag the gated second optimization pass (see the field doc).
        self.binsearch_assume_emitted = true;
    }
}

// ===================================================================
// Small constant-trip `while`-loop full-unroll hinting
// ===================================================================
//
// karac runs the same `default<O2>` LLVM pipeline as clang, yet on
// kata:37 (the sudoku backtracker, B-2026-06-17-7) the hot inner loop
// `while d <= 9 { ... if go(..) ..; d = d + 1 }` stayed ROLLED while
// rustc fully unrolled its byte-identical loop — and that single
// difference was the entire ~1.34x gap to Rust (proven: forcing the
// unroll via `#pragma clang loop unroll(full)` on the C mirror lands it
// exactly on Rust, 189.8ms; disabling rustc's unroller drops Rust back
// onto Kara/C at ~255ms). clang's default cost model DECLINES this
// unroll (recursive call + early exit make the body look expensive)
// even at `-O3 -funroll-loops`; rustc's LLVM config takes it. We close
// the gap the same way rustc effectively does: attach
// `llvm.loop.unroll.full` metadata to the back-edge of small,
// constant-upper-bounded counted loops so LLVM's full unroller fires.
//
// The hint is advisory-SAFE: unrolling never changes semantics, and if
// LLVM cannot actually prove a small constant trip count it ignores the
// metadata (so a mis-detected loop costs nothing, never correctness).
// The eligibility gate below only narrows WHICH loops carry the hint,
// to keep code size in check — LLVM remains the backstop.

/// Upper bound on the guard literal `K` in `v < K` / `v <= K` for a loop
/// to be hinted for full unroll. 9 (kata:37's candidate loop) sits well
/// under this; the 81-trip copy/signature loops in the same bench sit
/// above it and are left to the vectorizer, which serves them better.
const UNROLL_FULL_MAX_BOUND: i64 = 32;

/// `(var, K)` when `cond` upper-bounds a bare induction identifier by a
/// non-negative integer literal: `v < K`, `v <= K`, `K > v`, `K >= v`
/// — in the surface `Binary` form and the trait-method-lowered `Call`
/// form (mirror of `walk_guard_conjuncts`). Lower-bounded / decreasing
/// shapes (`v > K`, `K < v`) are intentionally rejected: there the
/// literal does not bound the trip count (the unseen init does), so the
/// hint would be meaningless.
fn guard_upper_bounded_counter(cond: &Expr) -> Option<(String, i64)> {
    // Surface Binary form.
    if let ExprKind::Binary { op, left, right } = &cond.kind {
        return match (op, &left.kind, &right.kind) {
            (BinOp::Lt | BinOp::LtEq, ExprKind::Identifier(v), ExprKind::Integer(k, _)) => {
                Some((v.clone(), *k))
            }
            (BinOp::Gt | BinOp::GtEq, ExprKind::Integer(k, _), ExprKind::Identifier(v)) => {
                Some((v.clone(), *k))
            }
            _ => None,
        };
    }
    // Trait-method-lowered Call form: `i64::lt(v, K)` etc.
    if let ExprKind::Call { callee, args } = &cond.kind {
        let ExprKind::Path { segments, .. } = &callee.kind else {
            return None;
        };
        if segments.len() != 2 || args.len() != 2 {
            return None;
        }
        let (a, b) = (&args[0].value.kind, &args[1].value.kind);
        return match (segments[1].as_str(), a, b) {
            ("lt" | "le", ExprKind::Identifier(v), ExprKind::Integer(k, _)) => {
                Some((v.clone(), *k))
            }
            ("gt" | "ge", ExprKind::Integer(k, _), ExprKind::Identifier(v)) => {
                Some((v.clone(), *k))
            }
            _ => None,
        };
    }
    None
}

/// True iff `body`'s top-level statements update `var` by a constant
/// step (`var = var + c` / `var += c`, or the `-` forms) at least once
/// AND never reassign it any other way. A top-level-only scan is
/// deliberate: a step buried in a nested branch isn't every-iteration,
/// so we conservatively decline (miss the unroll rather than over-apply
/// it). A non-step top-level reassignment disqualifies outright — that
/// breaks the constant-stride trip count.
fn body_top_level_constant_step(body: &Block, var: &str) -> bool {
    let mut saw_step = false;
    for stmt in &body.stmts {
        match &stmt.kind {
            StmtKind::Assign { target, value } => {
                if let ExprKind::Identifier(t) = &target.kind {
                    if t == var {
                        if classify_monotone_rhs(var, value).is_some() {
                            saw_step = true;
                        } else {
                            return false;
                        }
                    }
                }
            }
            StmtKind::CompoundAssign { target, op, value } => {
                if let ExprKind::Identifier(t) = &target.kind {
                    if t == var {
                        let is_step = matches!(op, CompoundOp::Add | CompoundOp::Sub)
                            && matches!(value.kind, ExprKind::Integer(_, _));
                        if is_step {
                            saw_step = true;
                        } else {
                            return false;
                        }
                    }
                }
            }
            _ => {}
        }
    }
    saw_step
}

/// `var` when `cond` upper-bounds a bare induction identifier — `v < X`,
/// `v <= X`, `X > v`, `X >= v` — with `X` ANY expression (unlike
/// `guard_upper_bounded_counter`, which needs a literal `X`). Used by the
/// partial-unroll gate, where the bound is typically a runtime value
/// (`while i <= n`). Both the surface `Binary` form and the trait-method-
/// lowered `Call` form are recognised.
fn guard_counter_var(cond: &Expr) -> Option<String> {
    if let ExprKind::Binary { op, left, right } = &cond.kind {
        return match (op, &left.kind, &right.kind) {
            (BinOp::Lt | BinOp::LtEq, ExprKind::Identifier(v), _) => Some(v.clone()),
            (BinOp::Gt | BinOp::GtEq, _, ExprKind::Identifier(v)) => Some(v.clone()),
            _ => None,
        };
    }
    if let ExprKind::Call { callee, args } = &cond.kind {
        let ExprKind::Path { segments, .. } = &callee.kind else {
            return None;
        };
        if segments.len() != 2 || args.len() != 2 {
            return None;
        }
        return match (
            segments[1].as_str(),
            &args[0].value.kind,
            &args[1].value.kind,
        ) {
            ("lt" | "le", ExprKind::Identifier(v), _) => Some(v.clone()),
            ("gt" | "ge", _, ExprKind::Identifier(v)) => Some(v.clone()),
            _ => None,
        };
    }
    None
}

/// Fail-closed "is this a pure-scalar (register-only) expression": literals,
/// identifiers, and arithmetic/logical `Binary`/`Unary` over the same. ANY
/// other shape — `Index`, `Call`, `MethodCall`, `FieldAccess`, string/heap
/// literals — returns false. This is the gate that keeps partial unroll off
/// MEMORY-bound loops (array scans like kata #63's dp loop), which forcing an
/// unroll count only bloats (B-2026-07-08-24).
fn expr_is_scalar_only(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Integer(_, _)
        | ExprKind::Float(_, _)
        | ExprKind::Bool(_)
        | ExprKind::CharLit(_)
        | ExprKind::ByteLit(_)
        | ExprKind::Identifier(_) => true,
        ExprKind::Binary { left, right, .. } => {
            expr_is_scalar_only(left) && expr_is_scalar_only(right)
        }
        ExprKind::Unary { operand, .. } => expr_is_scalar_only(operand),
        // The typechecker lowers primitive operators to trait-method Calls —
        // `a + b` → `i64::add(a, b)`, `i + 1` → `i64::add(i, 1)` (same rewrite
        // as the comparison Call form `walk_guard_conjuncts` handles). Accept a
        // 2-segment `<prim>::<op>(scalar args…)` call as scalar; every other
        // call (heap method, user fn, `Vec.push`, …) is not.
        ExprKind::Call { callee, args } => {
            if let ExprKind::Path { segments, .. } = &callee.kind {
                if segments.len() == 2 && is_scalar_op_method(&segments[1]) {
                    return args.iter().all(|a| expr_is_scalar_only(&a.value));
                }
            }
            false
        }
        _ => false,
    }
}

/// The primitive operator names the typechecker's operator-to-trait-method
/// lowering produces: arithmetic, bitwise, and comparison. A call to one of
/// these on scalar operands is pure register work — no memory, no side
/// effect — so it keeps a loop body eligible for partial unroll.
fn is_scalar_op_method(name: &str) -> bool {
    matches!(
        name,
        "add"
            | "sub"
            | "mul"
            | "div"
            | "rem"
            | "neg"
            | "bitand"
            | "bitor"
            | "bitxor"
            | "shl"
            | "shr"
            | "not"
            | "eq"
            | "ne"
            | "lt"
            | "le"
            | "gt"
            | "ge"
    )
}

/// Fail-closed "is every top-level statement of `body` pure-scalar": `let`
/// with a scalar initializer, scalar `x = e` / `x ±= e`, or a scalar
/// expression statement. Any other statement (nested loop, `return`, a
/// call, an indexed write) declines. A top-level-only scan matches
/// `body_top_level_constant_step`: a loop whose body is entirely scalar
/// arithmetic + counter advance is the Fibonacci-recurrence shape partial
/// unroll wins on.
fn body_is_scalar_only(body: &Block) -> bool {
    body.stmts.iter().all(|stmt| match &stmt.kind {
        StmtKind::Let { value, .. } => expr_is_scalar_only(value),
        StmtKind::Assign { target, value } => {
            expr_is_scalar_only(target) && expr_is_scalar_only(value)
        }
        StmtKind::CompoundAssign { target, value, .. } => {
            expr_is_scalar_only(target) && expr_is_scalar_only(value)
        }
        StmtKind::Expr(e) => expr_is_scalar_only(e),
        _ => false,
    })
}

impl<'ctx> super::Codegen<'ctx> {
    /// Whether this `while` loop should carry `llvm.loop.unroll.full`
    /// metadata: a small, constant-upper-bounded counted loop whose
    /// induction variable advances by a constant step every iteration.
    /// See the section comment for the kata:37 motivation and the safety
    /// argument (advisory-only hint, LLVM is the trip-count backstop).
    pub(super) fn while_loop_wants_full_unroll(&self, cond: &Expr, body: &Block) -> bool {
        let Some((var, bound)) = guard_upper_bounded_counter(cond) else {
            return false;
        };
        if !(0..=UNROLL_FULL_MAX_BOUND).contains(&bound) {
            return false;
        }
        body_top_level_constant_step(body, &var)
    }

    /// Whether this `while` loop should carry `llvm.loop.unroll.count` (a
    /// PARTIAL unroll by a fixed factor): a counted loop — a counter var
    /// upper-bounded in the guard and advanced by a constant step every
    /// iteration — whose body is entirely PURE-SCALAR (register arithmetic,
    /// no memory/calls). That is the Fibonacci-recurrence shape (kata #70's
    /// `next = a + b`) where LLVM 18's cost model wrongly declines the
    /// unroll (a loop-carried scalar recurrence "looks" un-parallelizable),
    /// but forcing a 4× unroll amortizes the per-iteration branch overhead
    /// for a measured ~1.38× win (B-2026-07-08-24). MEMORY-bound loops
    /// (array scans — e.g. #63's dp) are excluded by `body_is_scalar_only`:
    /// forcing a count there only bloats them (the in-pipeline cost model
    /// serves them, and full re-optimization keeps them neutral). The
    /// constant-small-trip case is left to `while_loop_wants_full_unroll`,
    /// so the caller checks that first.
    pub(super) fn while_loop_wants_partial_unroll(&self, cond: &Expr, body: &Block) -> bool {
        let Some(var) = guard_counter_var(cond) else {
            return false;
        };
        body_top_level_constant_step(body, &var) && body_is_scalar_only(body)
    }

    /// Attach `!llvm.loop !{!self, !{!"llvm.loop.unroll.full"}}` to a
    /// loop's back-edge branch so LLVM's full unroller fires. Builds the
    /// self-referential loop-id node via the temporary-node + RAUW recipe
    /// (LLVM's canonical way to construct cyclic metadata through the C
    /// API). karac's first `llvm.loop` metadata emission — see
    /// `while_loop_wants_full_unroll` for when it's called.
    pub(super) fn attach_unroll_full_metadata(
        &self,
        branch: inkwell::values::InstructionValue<'ctx>,
    ) {
        use inkwell::values::AsValueRef;
        use llvm_sys::core::{
            LLVMGetMDKindIDInContext, LLVMMDNodeInContext2, LLVMMDStringInContext2,
            LLVMMetadataAsValue, LLVMSetMetadata,
        };
        use llvm_sys::debuginfo::{LLVMMetadataReplaceAllUsesWith, LLVMTemporaryMDNode};
        use std::os::raw::{c_char, c_uint};

        let ctx = self.context.raw();
        unsafe {
            // !{!"llvm.loop.unroll.full"}
            let key = b"llvm.loop.unroll.full";
            let prop_str = LLVMMDStringInContext2(ctx, key.as_ptr() as *const c_char, key.len());
            let mut prop_ops = [prop_str];
            let prop_node = LLVMMDNodeInContext2(ctx, prop_ops.as_mut_ptr(), prop_ops.len());

            // Self-referential loop id: forward-declare a temp as the
            // first operand, build the real node, then RAUW temp -> node
            // so operand 0 points back at the node itself.
            let temp = LLVMTemporaryMDNode(ctx, std::ptr::null_mut(), 0);
            let mut loop_ops = [temp, prop_node];
            let loop_id = LLVMMDNodeInContext2(ctx, loop_ops.as_mut_ptr(), loop_ops.len());
            LLVMMetadataReplaceAllUsesWith(temp, loop_id);

            // branch !llvm.loop !loop_id
            let kind = b"llvm.loop";
            let kind_id =
                LLVMGetMDKindIDInContext(ctx, kind.as_ptr() as *const c_char, kind.len() as c_uint);
            let loop_id_val = LLVMMetadataAsValue(ctx, loop_id);
            LLVMSetMetadata(branch.as_value_ref(), kind_id, loop_id_val);
        }
    }

    /// Attach `!llvm.loop !{!self, !{!"llvm.loop.unroll.count", i32 count}}`
    /// to a loop's back-edge branch — a PARTIAL unroll by a fixed factor
    /// (vs `attach_unroll_full_metadata`'s full unroll). Same self-referential
    /// loop-id recipe; the only difference is the property node carries the
    /// `unroll.count` key plus an `i32` operand. See
    /// `while_loop_wants_partial_unroll` for when it fires.
    pub(super) fn attach_unroll_count_metadata(
        &self,
        branch: inkwell::values::InstructionValue<'ctx>,
        count: u32,
    ) {
        use inkwell::values::AsValueRef;
        use llvm_sys::core::{
            LLVMGetMDKindIDInContext, LLVMMDNodeInContext2, LLVMMDStringInContext2,
            LLVMMetadataAsValue, LLVMSetMetadata, LLVMValueAsMetadata,
        };
        use llvm_sys::debuginfo::{LLVMMetadataReplaceAllUsesWith, LLVMTemporaryMDNode};
        use std::os::raw::{c_char, c_uint};

        let ctx = self.context.raw();
        let count_const = self.context.i32_type().const_int(count as u64, false);
        unsafe {
            // !{!"llvm.loop.unroll.count", i32 count}
            let key = b"llvm.loop.unroll.count";
            let prop_str = LLVMMDStringInContext2(ctx, key.as_ptr() as *const c_char, key.len());
            let count_md = LLVMValueAsMetadata(count_const.as_value_ref());
            let mut prop_ops = [prop_str, count_md];
            let prop_node = LLVMMDNodeInContext2(ctx, prop_ops.as_mut_ptr(), prop_ops.len());

            let temp = LLVMTemporaryMDNode(ctx, std::ptr::null_mut(), 0);
            let mut loop_ops = [temp, prop_node];
            let loop_id = LLVMMDNodeInContext2(ctx, loop_ops.as_mut_ptr(), loop_ops.len());
            LLVMMetadataReplaceAllUsesWith(temp, loop_id);

            let kind = b"llvm.loop";
            let kind_id =
                LLVMGetMDKindIDInContext(ctx, kind.as_ptr() as *const c_char, kind.len() as c_uint);
            let loop_id_val = LLVMMetadataAsValue(ctx, loop_id);
            LLVMSetMetadata(branch.as_value_ref(), kind_id, loop_id_val);
        }
    }
}
