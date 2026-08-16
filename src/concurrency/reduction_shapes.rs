//! Reduction-shape recognition: accumulator-update forms, min/max
//! conditionals, lowered op-calls, binding-decl and callee-name walks.
//!
//! Extracted verbatim from `concurrency.rs` (structural-debt extraction,
//! 2026-08-16); helpers are `pub(super)` — this is a private submodule of
//! the concurrency pass, not API.

use super::*;

/// Pull the name out of a bare-identifier expression. Used by the
/// reduction recognizer to reject any assignment whose target is a
/// field access, index, or compound shape — those aren't a single
/// scalar accumulator and the fan-out / combine lowering doesn't cover
/// them at v1.
pub(super) fn identifier_name(expr: &Expr) -> Option<String> {
    match &expr.kind {
        ExprKind::Identifier(name) => Some(name.clone()),
        _ => None,
    }
}

/// True if `expr` is an integer literal — used to recognize the loop-
/// counter shape `i += 1` / `i = i + 1` and exclude it from the
/// reduction accumulator count. Floats are intentionally rejected here:
/// a float loop counter is unusual and the loop-counter excuse only
/// applies to integer steps anyway.
pub(super) fn is_int_literal(expr: &Expr) -> bool {
    matches!(expr.kind, ExprKind::Integer(_, _))
}

/// True if `value` has shape `acc + int_literal` for the named `acc` —
/// the loop-counter step pattern in an explicit `while` loop. Folded
/// alongside reduction-shape writes so kata-7-style benches (`while k <
/// K { sum = sum + ...; k = k + 1; }`) classify cleanly without
/// forcing the loop counter through the reduction allow-list.
///
/// Accepts both the pre-lowered `Binary` shape and the lowered
/// `Call(Path([type, "add"]), [a, b])` shape (`src/lowering.rs`
/// rewrites every primitive binop into a method-call dispatch before
/// the CLI runs concurrencycheck — without the second arm, the
/// recognizer fires only for the test pipeline that skips lowering).
/// The loop's OWN induction variable, when it has one: a `for` pattern
/// binding, or the variable tested by a `while k < end` condition.
///
/// This is what lets `classify_loop_body` tell a loop counter apart from an
/// accumulator that merely shares its shape (B-2026-08-11-16). Returning
/// `None` is always safe: every literal-step write is then treated as a
/// reduction candidate, so a loop with a counter this cannot name declines the
/// lowering rather than dropping a variable. `parse_lt_condition` is the same
/// matcher the cost model and the fan-out lowering use, and it accepts both the
/// pre-lowering `Binary { Lt, .. }` and the post-lowering `Call(.., "lt")`
/// spellings — so the answer here agrees with the shape codegen will actually
/// lower.
pub(super) fn loop_induction_var(expr: &Expr) -> Option<String> {
    match &expr.kind {
        ExprKind::For { pattern, .. } => match &pattern.kind {
            PatternKind::Binding(name) => Some(name.clone()),
            _ => None,
        },
        ExprKind::While { condition, .. } => {
            crate::par_cost::parse_lt_condition(condition).map(|(name, _)| name)
        }
        _ => None,
    }
}

pub(super) fn induction_step_via_assign(value: &Expr, acc_name: &str) -> bool {
    match &value.kind {
        ExprKind::Binary {
            op: BinOp::Add,
            left,
            right,
        } => is_acc_plus_int_literal(&left.kind, &right.kind, acc_name),
        ExprKind::Call { callee, args } => {
            match_lowered_op_call(callee, args, "add").is_some_and(|(left, right)| {
                is_acc_plus_int_literal(&left.kind, &right.kind, acc_name)
            })
        }
        _ => false,
    }
}

pub(super) fn is_acc_plus_int_literal(left: &ExprKind, right: &ExprKind, acc_name: &str) -> bool {
    match (left, right) {
        (ExprKind::Identifier(n), ExprKind::Integer(_, _))
        | (ExprKind::Integer(_, _), ExprKind::Identifier(n)) => n == acc_name,
        _ => false,
    }
}

/// True if `value` has shape `acc <op> expr` or `expr <op> acc` for
/// the named `acc` and `op` in the reduction allow-list — the right-
/// hand side of `acc = acc <op> expr`. Returns the op kind on match.
/// Commutativity is exploited at recognition: an allow-list op `+/*/|/&/^`
/// is commutative, so the analyzer accepts `acc op expr` and `expr op
/// acc` symmetrically. The right-hand `expr` is unconstrained — any
/// shape that produces a value combinable with `acc` is fine; the
/// codegen slice will type-gate.
///
/// Like `induction_step_via_assign`, this checks both the pre-lowered
/// `Binary` and the lowered `Call(Path([type, op_method]), [a, b])`
/// shapes — see that function's doc comment for context.
/// Does this type annotation name a positionally-indexed sequence?
///
/// `Vec[T]`, `Slice[T]`, `mut Slice[T]`, and `[T; N]` qualify. `Map` / `Set`
/// and their sorted / hash spellings do not — their element syntax is a hash
/// (or tree) insert, not a slot store. Borrow wrappers are transparent.
pub(super) fn type_expr_is_sequence(ty: &TypeExpr) -> bool {
    match &ty.kind {
        TypeKind::Array { .. } | TypeKind::MutSlice(_) => true,
        TypeKind::Ref(inner) | TypeKind::MutRef(inner) => type_expr_is_sequence(inner),
        TypeKind::Path(p) => matches!(p.segments.last().map(String::as_str), Some("Vec" | "Slice")),
        _ => false,
    }
}

/// Does this initializer build a positionally-indexed sequence? Used only for
/// an UNANNOTATED `let`, where the annotation channel has nothing to read.
pub(super) fn init_expr_builds_sequence(init: &Expr) -> bool {
    match &init.kind {
        ExprKind::ArrayLiteral(_) | ExprKind::RepeatLiteral { .. } => true,
        ExprKind::PrefixCollectionLiteral { type_name, .. } => type_name == "Vec",
        // `Vec.new()` / `Vec.filled(n, x)` / `Vec.with_capacity(n)`.
        ExprKind::Call { callee, .. } => match &callee.kind {
            ExprKind::Path { segments, .. } => segments.len() == 2 && segments[0] == "Vec",
            _ => false,
        },
        _ => false,
    }
}

/// Find `name`'s `let` declaration anywhere in `block`, returning its
/// annotation and initializer. First match wins — a shadowing rebind would make
/// the answer ambiguous, and the sequence check treats ambiguity as "no".
pub(super) fn find_binding_decl<'a>(
    block: &'a Block,
    name: &str,
) -> Option<(Option<&'a TypeExpr>, Option<&'a Expr>)> {
    for stmt in &block.stmts {
        match &stmt.kind {
            StmtKind::Let {
                pattern, ty, value, ..
            }
            | StmtKind::LetElse {
                pattern, ty, value, ..
            } => {
                if pattern.binding_names().iter().any(|n| n == name) {
                    return Some((ty.as_ref(), Some(value)));
                }
            }
            StmtKind::LetUninit { name: n, ty, .. } if n == name => {
                return Some((Some(ty), None));
            }
            _ => {}
        }
        if let Some(hit) = find_binding_decl_in_stmt(stmt, name) {
            return Some(hit);
        }
    }
    block
        .final_expr
        .as_ref()
        .and_then(|e| find_binding_decl_in_expr(e, name))
}

pub(super) fn find_binding_decl_in_stmt<'a>(
    stmt: &'a Stmt,
    name: &str,
) -> Option<(Option<&'a TypeExpr>, Option<&'a Expr>)> {
    match &stmt.kind {
        StmtKind::Let { value, .. } => find_binding_decl_in_expr(value, name),
        StmtKind::LetElse {
            value, else_block, ..
        } => find_binding_decl_in_expr(value, name).or_else(|| find_binding_decl(else_block, name)),
        StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
            find_binding_decl_in_expr(target, name)
                .or_else(|| find_binding_decl_in_expr(value, name))
        }
        StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => find_binding_decl(body, name),
        StmtKind::Expr(e) => find_binding_decl_in_expr(e, name),
        StmtKind::LetUninit { .. } | StmtKind::MultiAssign { .. } => None,
    }
}

pub(super) fn find_binding_decl_in_expr<'a>(
    expr: &'a Expr,
    name: &str,
) -> Option<(Option<&'a TypeExpr>, Option<&'a Expr>)> {
    let mut found: Option<(Option<&'a TypeExpr>, Option<&'a Expr>)> = None;
    crate::index_disjoint::for_each_child_public(expr, &mut |c| {
        if found.is_some() {
            return;
        }
        found = match c {
            crate::index_disjoint::Child::Expr(e) => find_binding_decl_in_expr(e, name),
            crate::index_disjoint::Child::Block(b) => find_binding_decl(b, name),
        };
    });
    found
}

/// The OBJECT expression of every `name[...] = ...` / `name[...] op= ...` in
/// `block`, at any nesting depth. Spans on those objects key the typechecker's
/// `expr_types`, which is how a write target's container type is recovered.
pub(super) fn collect_index_assign_objects_in_block<'a>(block: &'a Block, out: &mut Vec<&'a Expr>) {
    for stmt in &block.stmts {
        if let StmtKind::Assign { target, .. } | StmtKind::CompoundAssign { target, .. } =
            &stmt.kind
        {
            if let ExprKind::Index { object, .. } = &target.kind {
                out.push(object);
            }
        }
        match &stmt.kind {
            StmtKind::Let { value, .. } => collect_index_assign_objects_in_expr(value, out),
            StmtKind::LetElse {
                value, else_block, ..
            } => {
                collect_index_assign_objects_in_expr(value, out);
                collect_index_assign_objects_in_block(else_block, out);
            }
            StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
                collect_index_assign_objects_in_expr(target, out);
                collect_index_assign_objects_in_expr(value, out);
            }
            StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                collect_index_assign_objects_in_block(body, out)
            }
            StmtKind::Expr(e) => collect_index_assign_objects_in_expr(e, out),
            StmtKind::LetUninit { .. } | StmtKind::MultiAssign { .. } => {}
        }
    }
    if let Some(e) = &block.final_expr {
        collect_index_assign_objects_in_expr(e, out);
    }
}

pub(super) fn collect_index_assign_objects_in_expr<'a>(expr: &'a Expr, out: &mut Vec<&'a Expr>) {
    crate::index_disjoint::for_each_child_public(expr, &mut |c| match c {
        crate::index_disjoint::Child::Expr(e) => collect_index_assign_objects_in_expr(e, out),
        crate::index_disjoint::Child::Block(b) => collect_index_assign_objects_in_block(b, out),
    });
}

/// Bare callee names reachable from `block` — free-function calls and method
/// names, at any nesting depth. Mirrors `StmtInfo::called_fn_names`' keying
/// (bare name for a free fn, bare method name for a method) so the same
/// `function_bodies` / `method_bodies` lookups apply.
///
/// Over-collecting is safe for its one consumer
/// (`block_emits_output_transitively`): a name that resolves to nothing is
/// treated as output-emitting, which declines a fan-out rather than admitting
/// an unsound one.
pub(super) fn collect_callee_names_in_block(block: &Block, out: &mut HashSet<String>) {
    for stmt in &block.stmts {
        match &stmt.kind {
            StmtKind::Let { value, .. } => collect_callee_names_in_expr(value, out),
            StmtKind::LetElse {
                value, else_block, ..
            } => {
                collect_callee_names_in_expr(value, out);
                collect_callee_names_in_block(else_block, out);
            }
            StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
                collect_callee_names_in_expr(target, out);
                collect_callee_names_in_expr(value, out);
            }
            StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                collect_callee_names_in_block(body, out)
            }
            StmtKind::Expr(e) => collect_callee_names_in_expr(e, out),
            StmtKind::LetUninit { .. } | StmtKind::MultiAssign { .. } => {}
        }
    }
    if let Some(e) = &block.final_expr {
        collect_callee_names_in_expr(e, out);
    }
}

pub(super) fn collect_callee_names_in_expr(expr: &Expr, out: &mut HashSet<String>) {
    if let ExprKind::Call { callee, .. } = &expr.kind {
        match &callee.kind {
            ExprKind::Identifier(n) => {
                out.insert(n.clone());
            }
            ExprKind::Path { segments, .. } => {
                // `Type.method` associated calls key as `Type.method` in
                // `method_bodies`; a single-segment path is a free fn.
                out.insert(if segments.len() >= 2 {
                    format!(
                        "{}.{}",
                        segments[segments.len() - 2],
                        segments[segments.len() - 1]
                    )
                } else if let Some(last) = segments.last() {
                    last.clone()
                } else {
                    return;
                });
            }
            _ => {}
        }
    }
    if let ExprKind::MethodCall { method, .. } = &expr.kind {
        out.insert(method.clone());
    }
    crate::index_disjoint::for_each_child_public(expr, &mut |c| match c {
        crate::index_disjoint::Child::Expr(e) => collect_callee_names_in_expr(e, out),
        crate::index_disjoint::Child::Block(b) => collect_callee_names_in_block(b, out),
    });
}

/// Can a callee that resolves to no source-defined function still write to the
/// console?
///
/// Only the console writers can: the free `println` family (already caught
/// syntactically by `block_has_console_output`, listed here so the two spellings
/// agree) and the `Stdout` / `Stderr` writer methods. Everything else that fails
/// to resolve is stdlib — `push`, `len`, `sort`, a lowered `i64.mul` — and is
/// silent.
///
/// The inverted default matters: an allow-list of silent builtins would have to
/// enumerate the whole stdlib, and every name it missed would decline a loop for
/// no reason (measured: `tmp.push(v)` alone was enough).
///
/// **Known limit.** An `extern` function could print without declaring an
/// effect, and nothing here would see it. That is the same resourceless-console
/// blind spot the rest of this pass carries (see `stmt_has_console_output`); an
/// extern whose declared effects don't describe what it does is already outside
/// what any analysis in this file can promise.
pub(super) fn unresolved_callee_may_print(name: &str) -> bool {
    matches!(
        name,
        "println" | "print" | "eprintln" | "eprint" | "write" | "write_line" | "writeln" | "flush"
    )
}

/// The loop variable name to report for a disjoint-write candidate, for the
/// declined case where the proof never got far enough to return one. Empty
/// string when the loop has no simple binding (a `while`, a destructuring
/// `for`), which is itself one of the decline reasons.
pub(super) fn disjoint_candidate_loop_var(loop_expr: &Expr) -> Option<String> {
    let ExprKind::For { pattern, .. } = &loop_expr.kind else {
        return None;
    };
    match &pattern.kind {
        PatternKind::Binding(name) => Some(name.clone()),
        _ => None,
    }
}

pub(super) fn reduction_binary_shape(value: &Expr, acc_name: &str) -> Option<ReductionOp> {
    match &value.kind {
        ExprKind::Binary { op, left, right } => {
            let red_op = ReductionOp::from_bin_op(op)?;
            // Direct shape: `acc <op> expr` or `expr <op> acc`.
            if acc_matches_either(&left.kind, &right.kind, acc_name) {
                return Some(red_op);
            }
            // Nested chain: `acc + a + b` parses left-associatively as
            // `Binary(+, Binary(+, acc, a), b)` — the direct match
            // above sees neither operand as the acc identifier. By
            // commutativity of the allow-list ops, any chain of the
            // same op containing the accumulator exactly once is a
            // valid reduction step: reorder to `acc + (others-combined)`
            // and the recognized reduction shape falls out. Count acc
            // occurrences across the same-op chain; recognize iff it
            // appears exactly once.
            if count_acc_in_chain(value, op, op_method_for_bin_op(op), acc_name) == 1 {
                return Some(red_op);
            }
            None
        }
        ExprKind::Call { callee, args } => {
            let ExprKind::Path { segments, .. } = &callee.kind else {
                return None;
            };
            if segments.len() != 2 || args.len() != 2 {
                return None;
            }
            let red_op = match segments[1].as_str() {
                "add" => ReductionOp::Add,
                "mul" => ReductionOp::Mul,
                "bitor" => ReductionOp::BitOr,
                "bitand" => ReductionOp::BitAnd,
                "bitxor" => ReductionOp::BitXor,
                "min" => ReductionOp::Min,
                "max" => ReductionOp::Max,
                _ => return None,
            };
            // Direct shape: `T.op(acc, expr)` / `T.op(expr, acc)`.
            if acc_matches_either(&args[0].value.kind, &args[1].value.kind, acc_name) {
                return Some(red_op);
            }
            // Post-lowering chain — mirror of the Binary branch above
            // but for the `Call(Path([T, op_method]), [a, b])` shape the
            // lowering pass emits. Use the bin-op corresponding to
            // segments[1] so the chain walker recognizes both pre- and
            // post-lowering nodes uniformly.
            let chain_op = bin_op_for_op_method(segments[1].as_str())?;
            if count_acc_in_chain(value, &chain_op, segments[1].as_str(), acc_name) == 1 {
                return Some(red_op);
            }
            None
        }
        _ => None,
    }
}

/// Count occurrences of `acc_name` (as a leaf `Identifier`) in a chain
/// of nested expressions where each level is either a `Binary(op, ...)`
/// matching `target_op` or a `Call(Path([_, target_method]), [...])`
/// matching `target_method`. Recursion stops at any expression that's
/// not a same-op chain node (those count as leaves and contribute 1
/// iff they're the acc identifier, else 0).
///
/// Used by `reduction_binary_shape` to recognize commutative-reduction
/// chains like `acc + a + b` (parses as `Binary(+, Binary(+, acc, a),
/// b)`) — any chain of the same allow-list op containing acc exactly
/// once is a valid reduction step under commutativity, since the chain
/// can be reordered to `acc + (others-combined)`.
pub(super) fn count_acc_in_chain(
    expr: &Expr,
    target_op: &BinOp,
    target_method: &str,
    acc_name: &str,
) -> usize {
    match &expr.kind {
        ExprKind::Binary { op, left, right } if op == target_op => {
            count_acc_in_chain(left, target_op, target_method, acc_name)
                + count_acc_in_chain(right, target_op, target_method, acc_name)
        }
        ExprKind::Call { callee, args } if args.len() == 2 => {
            if let ExprKind::Path { segments, .. } = &callee.kind {
                if segments.len() == 2 && segments[1] == target_method {
                    return count_acc_in_chain(&args[0].value, target_op, target_method, acc_name)
                        + count_acc_in_chain(&args[1].value, target_op, target_method, acc_name);
                }
            }
            0
        }
        ExprKind::Identifier(n) if n == acc_name => 1,
        _ => 0,
    }
}

/// Map a `BinOp` to its lowered op-method name (`Add` → `"add"`, etc.).
/// Mirror of `ReductionOp::from_bin_op`'s op-method conventions; used
/// by the chain walker so it can match both pre-lowering Binary and
/// post-lowering Call nodes uniformly under the same chain.
pub(super) fn op_method_for_bin_op(op: &BinOp) -> &'static str {
    match op {
        BinOp::Add => "add",
        BinOp::Mul => "mul",
        BinOp::BitOr => "bitor",
        BinOp::BitAnd => "bitand",
        BinOp::BitXor => "bitxor",
        // Min/Max have no BinOp glyph — never recognized as chain
        // members through the Binary path. Falls back to a name that
        // won't match any Call segment.
        _ => "",
    }
}

pub(super) fn bin_op_for_op_method(method: &str) -> Option<BinOp> {
    match method {
        "add" => Some(BinOp::Add),
        "mul" => Some(BinOp::Mul),
        "bitor" => Some(BinOp::BitOr),
        "bitand" => Some(BinOp::BitAnd),
        "bitxor" => Some(BinOp::BitXor),
        // Min/Max are call-form only, no BinOp counterpart — the
        // chain walker still works through `target_method` matching
        // even though the BinOp side never fires.
        _ => None,
    }
}

pub(super) fn acc_matches_either(left: &ExprKind, right: &ExprKind, acc_name: &str) -> bool {
    matches!(left, ExprKind::Identifier(n) if n == acc_name)
        || matches!(right, ExprKind::Identifier(n) if n == acc_name)
}

/// Recognize a conditional-assign Min/Max reduction:
/// `if x < acc { acc = x; }` → Min, `if x > acc { acc = x; }` → Max
/// (with symmetric `acc > x` → Min and `acc < x` → Max accepted too).
///
/// Returns `Some((acc_name, op))` when the if-stmt shapes a Min/Max
/// reduction step against a single accumulator. The recognizer is
/// conservative — extends to richer assignment-RHS expressions in a
/// follow-up if a workload surfaces the shape:
/// - else-less if only (no `else` / `else-if` arms),
/// - body is exactly one statement, an `Assign` to an identifier target,
/// - assignment value is a single identifier (matches the kata-153
///   `let x = ...; if x < m { m = x; }` desugar pattern; richer RHS
///   like `if a[i] < m { m = a[i]; }` is not supported at v1),
/// - condition is `Binary(Lt | Gt)` (or the lowered `Call(Path([T, "lt"|"gt"]), [a, b])`)
///   with both operands as identifiers, one matching the assignment
///   target and the other matching the assignment value.
pub(super) fn conditional_minmax_shape(expr: &Expr) -> Option<(String, ReductionOp)> {
    let ExprKind::If {
        condition,
        then_block,
        else_branch,
    } = &expr.kind
    else {
        return None;
    };
    if else_branch.is_some() {
        return None;
    }
    if then_block.stmts.len() != 1 || then_block.final_expr.is_some() {
        return None;
    }
    let StmtKind::Assign { target, value } = &then_block.stmts[0].kind else {
        return None;
    };
    let acc_name = identifier_name(target)?;
    let ExprKind::Identifier(value_name) = &value.kind else {
        return None;
    };
    let (cmp_op, left, right) = match &condition.kind {
        ExprKind::Binary { op, left, right } => (op.clone(), left.as_ref(), right.as_ref()),
        ExprKind::Call { callee, args } if args.len() == 2 => {
            let ExprKind::Path { segments, .. } = &callee.kind else {
                return None;
            };
            if segments.len() != 2 {
                return None;
            }
            let op = match segments[1].as_str() {
                "lt" => BinOp::Lt,
                "gt" => BinOp::Gt,
                _ => return None,
            };
            (op, &args[0].value, &args[1].value)
        }
        _ => return None,
    };
    let ExprKind::Identifier(l_name) = &left.kind else {
        return None;
    };
    let ExprKind::Identifier(r_name) = &right.kind else {
        return None;
    };
    // `value < acc` → Min (new value is smaller, picked into acc).
    // `acc > value` → Min (commutative re-arrangement).
    // `value > acc` → Max, `acc < value` → Max (mirror).
    let red_op = match cmp_op {
        BinOp::Lt => {
            if l_name == value_name && r_name == &acc_name {
                ReductionOp::Min
            } else if l_name == &acc_name && r_name == value_name {
                ReductionOp::Max
            } else {
                return None;
            }
        }
        BinOp::Gt => {
            if l_name == value_name && r_name == &acc_name {
                ReductionOp::Max
            } else if l_name == &acc_name && r_name == value_name {
                ReductionOp::Min
            } else {
                return None;
            }
        }
        _ => return None,
    };
    Some((acc_name, red_op))
}

/// Match `Call(Path([type, method_name]), [a, b])` and return the two
/// arg expressions. Used by both `reduction_binary_shape` and
/// `induction_step_via_assign` to peek at the operand positions of a
/// post-lowering primitive op call.
pub(super) fn match_lowered_op_call<'a>(
    callee: &Expr,
    args: &'a [crate::ast::CallArg],
    method_name: &str,
) -> Option<(&'a Expr, &'a Expr)> {
    let ExprKind::Path { segments, .. } = &callee.kind else {
        return None;
    };
    if segments.len() != 2 || segments[1] != method_name || args.len() != 2 {
        return None;
    }
    Some((&args[0].value, &args[1].value))
}

/// Classify a single statement as a recognized accumulator update for a
/// reduction step: `acc = acc <op> EXPR` (Assign), `acc OP= EXPR`
/// (CompoundAssign), or chain shapes accepted by `reduction_binary_shape`.
/// Returns `Some((acc_name, op))` on a match; `None` otherwise.
///
/// Shared by both arms of `conditional_acc_update_shape` so the 2-arm
/// case can re-classify the else-arm with the same rules. The
/// unconditional `acc += const_lit` induction-step shape is
/// special-cased upstream in `classify_loop_body`'s CompoundAssign arm
/// (treated as the loop counter); under a conditional wrap the same
/// syntactic shape means "count of truthy iterations" and is a
/// legitimate reduction, so we do not bail here.
pub(super) fn single_stmt_as_acc_update(stmt: &Stmt) -> Option<(String, ReductionOp)> {
    match &stmt.kind {
        StmtKind::Assign { target, value } => {
            let name = identifier_name(target)?;
            let op = reduction_binary_shape(value, &name)?;
            Some((name, op))
        }
        StmtKind::CompoundAssign { target, op, .. } => {
            let name = identifier_name(target)?;
            ReductionOp::from_compound_op(op).map(|red_op| (name, red_op))
        }
        _ => None,
    }
}

/// Same as [`single_stmt_as_acc_update`] but wrapping a `Block` that
/// must contain exactly one statement and no trailing expression.
pub(super) fn single_stmt_block_as_acc_update(block: &Block) -> Option<(String, ReductionOp)> {
    if block.stmts.len() != 1 || block.final_expr.is_some() {
        return None;
    }
    single_stmt_as_acc_update(&block.stmts[0])
}

/// Recognize the collect-step shape: `acc.push(EXPR)` where `acc` is a
/// bare identifier (no field / index / chain receivers). Returns
/// `Some(acc_name)` on a match; `None` otherwise.
///
/// Generic-arg lists on `push` (`acc.push[T](x)`) are accepted only with
/// no args — the `push` method has no useful generic args today; the
/// matcher is shape-only and doesn't validate `acc`'s type. The codegen
/// layer (Phase 3) is responsible for confirming `acc: Vec[T]` /
/// `String` / similar; non-matching types fall through to sequential
/// code as a natural consequence of the codegen-side type check.
///
/// The single-arg requirement is the canonical `Vec::push(x)` shape; if
/// future workloads need `push_many(values)` or other multi-arg
/// collectors, the matcher can be extended.
pub(crate) fn collect_push_shape(expr: &Expr) -> Option<String> {
    let ExprKind::MethodCall {
        object,
        method,
        args,
        ..
    } = &expr.kind
    else {
        return None;
    };
    if method != "push" || args.len() != 1 {
        return None;
    }
    let ExprKind::Identifier(name) = &object.kind else {
        return None;
    };
    Some(name.clone())
}
