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

use super::state::AssertedIndexBound;

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
            ExprKind::Identifier(name) => self.len_alias.get(name.as_str()).cloned(),
            _ => None,
        }
    }
}
