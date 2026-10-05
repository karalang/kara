//! B-2026-09-29-41 — rename an arm binding that shadows its own scrutinee.
//!
//! `match s { Some(S { r, s }) => r.id, None => 0 }` binds a NEW `s` inside
//! the arm while the scrutinee is the OLD one. The interpreter keeps the two
//! apart; the AOT backend did not, because everything it does to the
//! scrutinee AFTER the arm's bindings exist looks the scrutinee up BY NAME —
//! the payload disarms, the boxed-field suppress, the leaf-drop retraction,
//! every per-variable sidecar map. With the binding installed under the same
//! name, those lookups found the binding: the boxed-field suppress wrote the
//! `Option`'s w0 through the arm's `String` slot (a segfault), and the
//! neighbouring spellings double-freed or lost a `Drop` body. `bind_pattern`
//! also purges the name's metadata on a rebind (`shadow.rs`), so the
//! scrutinee's class tags were gone for whatever read it after the match.
//!
//! There is no by-name lookup to patch here: there are dozens, spread over
//! the match, `if let`, `while let` and `let … else` lowerings, and each one
//! is right about every program where the names differ. So the names are
//! made to differ. Before any function compiles, every binding of that shape
//! is renamed to a fresh name in its pattern and in every use it reaches,
//! scope-aware (an inner rebinding of the same name stops the rename). The
//! outer binding keeps its name, so every table the checkers keyed by it is
//! untouched; only the arm-local name is new, and nothing outside the arm
//! could refer to it.
//!
//! Two exclusions keep the rename off names another phase's tables mention:
//! a name a closure or `par` block inside the arm mentions (ownership
//! publishes their capture lists by NAME), and a name the RC-fallback pass
//! promoted (`skip`).
//! Those keep today's lowering, which is no worse than before.

use std::cell::Cell;

use crate::ast::*;
use crate::binding_rename::{rename_pattern_binding, rename_uses_block, rename_uses_expr};
use crate::index_disjoint::{for_each_block_child, for_each_child_public, Child};

/// `program` with every scrutinee-shadowing binding renamed, or `None` when
/// there is none (the common case, which then costs no clone).
pub(super) fn rename_scrutinee_shadowing_bindings(
    program: &Program,
    skip: &dyn Fn(&str) -> bool,
) -> Option<Program> {
    let any = program.items.iter().any(|item| match item {
        Item::Function(f) => block_has_candidate(&f.body, skip),
        Item::ImplBlock(b) => b.items.iter().any(|ii| match ii {
            ImplItem::Method(f) => block_has_candidate(&f.body, skip),
            _ => false,
        }),
        _ => false,
    });
    if !any {
        return None;
    }
    let mut out = program.clone();
    let counter = Cell::new(0usize);
    for item in out.items.iter_mut() {
        match item {
            Item::Function(f) => rewrite_block(&mut f.body, skip, &counter),
            Item::ImplBlock(b) => {
                for ii in b.items.iter_mut() {
                    if let ImplItem::Method(f) = ii {
                        rewrite_block(&mut f.body, skip, &counter);
                    }
                }
            }
            _ => {}
        }
    }
    Some(out)
}

/// The scrutinee's name when `pattern` rebinds it.
fn shadowed_scrutinee<'a>(value: &'a Expr, pattern: &Pattern) -> Option<&'a str> {
    let ExprKind::Identifier(n) = &value.kind else {
        return None;
    };
    pattern
        .binding_names()
        .iter()
        .any(|b| b == n)
        .then_some(n.as_str())
}

/// Does a closure or `par` block inside `e` mention `name`? Their capture
/// lists are published by NAME, so a renamed binding they capture would miss.
fn expr_captures_by_name(e: &Expr, name: &str) -> bool {
    if matches!(e.kind, ExprKind::Closure { .. } | ExprKind::Par(_)) {
        return expr_mentions(e, name);
    }
    any_child(e, &mut |c| match c {
        Child::Expr(x) => expr_captures_by_name(x, name),
        Child::Block(b) => block_captures_by_name(b, name),
    })
}

fn block_captures_by_name(b: &Block, name: &str) -> bool {
    any_block_child(b, &mut |c| match c {
        Child::Expr(x) => expr_captures_by_name(x, name),
        Child::Block(b) => block_captures_by_name(b, name),
    })
}

fn expr_mentions(e: &Expr, name: &str) -> bool {
    if matches!(&e.kind, ExprKind::Identifier(n) if n == name) {
        return true;
    }
    any_child(e, &mut |c| match c {
        Child::Expr(x) => expr_mentions(x, name),
        Child::Block(b) => block_mentions(b, name),
    })
}

fn block_mentions(b: &Block, name: &str) -> bool {
    any_block_child(b, &mut |c| match c {
        Child::Expr(x) => expr_mentions(x, name),
        Child::Block(b) => block_mentions(b, name),
    })
}

fn any_child(e: &Expr, pred: &mut dyn FnMut(Child<'_>) -> bool) -> bool {
    let mut found = false;
    for_each_child_public(e, &mut |c| {
        if !found {
            found = pred(c);
        }
    });
    found
}

fn any_block_child(b: &Block, pred: &mut dyn FnMut(Child<'_>) -> bool) -> bool {
    let mut found = false;
    for_each_block_child(b, &mut |c| {
        if !found {
            found = pred(c);
        }
    });
    found
}

fn admissible(name: &str, scope: &[&Expr], blocks: &[&Block], skip: &dyn Fn(&str) -> bool) -> bool {
    !skip(name)
        && !scope.iter().any(|e| expr_captures_by_name(e, name))
        && !blocks.iter().any(|b| block_captures_by_name(b, name))
}

fn block_has_candidate(b: &Block, skip: &dyn Fn(&str) -> bool) -> bool {
    for (i, stmt) in b.stmts.iter().enumerate() {
        if let StmtKind::LetElse { pattern, value, .. } = &stmt.kind {
            if let Some(n) = shadowed_scrutinee(value, pattern) {
                let rest = Block {
                    stmts: b.stmts[i + 1..].to_vec(),
                    final_expr: b.final_expr.clone(),
                    span: b.span,
                };
                if admissible(n, &[], &[&rest], skip) {
                    return true;
                }
            }
        }
    }
    any_block_child(b, &mut |c| match c {
        Child::Expr(x) => expr_has_candidate(x, skip),
        Child::Block(b) => block_has_candidate(b, skip),
    })
}

fn expr_has_candidate(e: &Expr, skip: &dyn Fn(&str) -> bool) -> bool {
    let here = match &e.kind {
        ExprKind::Match { scrutinee, arms } => arms.iter().any(|arm| {
            shadowed_scrutinee(scrutinee, &arm.pattern).is_some_and(|n| {
                let mut scope = vec![&arm.body];
                scope.extend(arm.guard.as_ref());
                admissible(n, &scope, &[], skip)
            })
        }),
        ExprKind::IfLet {
            pattern,
            value,
            then_block,
            ..
        } => shadowed_scrutinee(value, pattern)
            .is_some_and(|n| admissible(n, &[], &[then_block], skip)),
        ExprKind::WhileLet {
            pattern,
            value,
            body,
            ..
        } => shadowed_scrutinee(value, pattern).is_some_and(|n| admissible(n, &[], &[body], skip)),
        _ => false,
    };
    if here {
        return true;
    }
    any_child(e, &mut |c| match c {
        Child::Expr(x) => expr_has_candidate(x, skip),
        Child::Block(b) => block_has_candidate(b, skip),
    })
}

fn fresh(name: &str, counter: &Cell<usize>) -> String {
    counter.set(counter.get() + 1);
    format!("{name}__arm_shadow{}", counter.get())
}

// ── The rewrite ────────────────────────────────────────────────

fn rewrite_block(b: &mut Block, skip: &dyn Fn(&str) -> bool, counter: &Cell<usize>) {
    let mut i = 0;
    while i < b.stmts.len() {
        let mut renamed: Option<(String, String)> = None;
        if let StmtKind::LetElse { pattern, value, .. } = &b.stmts[i].kind {
            if let Some(n) = shadowed_scrutinee(value, pattern) {
                let rest = Block {
                    stmts: b.stmts[i + 1..].to_vec(),
                    final_expr: b.final_expr.clone(),
                    span: b.span,
                };
                if admissible(n, &[], &[&rest], skip) {
                    renamed = Some((n.to_string(), fresh(n, counter)));
                }
            }
        }
        if let Some((from, to)) = renamed {
            if let StmtKind::LetElse { pattern, .. } = &mut b.stmts[i].kind {
                rename_pattern_binding(pattern, &from, &to);
            }
            // The binding's scope is the REST of the block.
            let mut rest = Block {
                stmts: b.stmts.split_off(i + 1),
                final_expr: b.final_expr.take(),
                span: b.span,
            };
            rename_uses_block(&mut rest, &from, &to);
            b.stmts.append(&mut rest.stmts);
            b.final_expr = rest.final_expr;
        }
        rewrite_stmt(&mut b.stmts[i], skip, counter);
        i += 1;
    }
    if let Some(fe) = b.final_expr.as_mut() {
        rewrite_expr(fe, skip, counter);
    }
}

fn rewrite_stmt(s: &mut Stmt, skip: &dyn Fn(&str) -> bool, counter: &Cell<usize>) {
    match &mut s.kind {
        StmtKind::Let { value, .. } => rewrite_expr(value, skip, counter),
        StmtKind::LetUninit { .. } => {}
        StmtKind::LetElse {
            value, else_block, ..
        } => {
            rewrite_expr(value, skip, counter);
            rewrite_block(else_block, skip, counter);
        }
        StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
            rewrite_block(body, skip, counter)
        }
        StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
            rewrite_expr(target, skip, counter);
            rewrite_expr(value, skip, counter);
        }
        StmtKind::MultiAssign { targets, values } => {
            for e in targets.iter_mut().chain(values.iter_mut()) {
                rewrite_expr(e, skip, counter);
            }
        }
        StmtKind::Expr(e) => rewrite_expr(e, skip, counter),
    }
}

fn rewrite_expr(e: &mut Expr, skip: &dyn Fn(&str) -> bool, counter: &Cell<usize>) {
    match &mut e.kind {
        ExprKind::Match { scrutinee, arms } => {
            rewrite_expr(scrutinee, skip, counter);
            for arm in arms.iter_mut() {
                if let Some(n) = shadowed_scrutinee(scrutinee, &arm.pattern) {
                    let mut scope = vec![&arm.body];
                    scope.extend(arm.guard.as_ref());
                    if admissible(n, &scope, &[], skip) {
                        let from = n.to_string();
                        let to = fresh(n, counter);
                        rename_pattern_binding(&mut arm.pattern, &from, &to);
                        if let Some(g) = arm.guard.as_mut() {
                            rename_uses_expr(g, &from, &to);
                        }
                        rename_uses_expr(&mut arm.body, &from, &to);
                    }
                }
                if let Some(g) = arm.guard.as_mut() {
                    rewrite_expr(g, skip, counter);
                }
                rewrite_expr(&mut arm.body, skip, counter);
            }
        }
        ExprKind::IfLet {
            pattern,
            value,
            then_block,
            else_branch,
        } => {
            rewrite_expr(value, skip, counter);
            if let Some(n) = shadowed_scrutinee(value, pattern) {
                if admissible(n, &[], &[then_block], skip) {
                    let from = n.to_string();
                    let to = fresh(n, counter);
                    rename_pattern_binding(pattern, &from, &to);
                    rename_uses_block(then_block, &from, &to);
                }
            }
            rewrite_block(then_block, skip, counter);
            if let Some(eb) = else_branch.as_mut() {
                rewrite_expr(eb, skip, counter);
            }
        }
        ExprKind::WhileLet {
            pattern,
            value,
            body,
            ..
        } => {
            rewrite_expr(value, skip, counter);
            if let Some(n) = shadowed_scrutinee(value, pattern) {
                if admissible(n, &[], &[body], skip) {
                    let from = n.to_string();
                    let to = fresh(n, counter);
                    rename_pattern_binding(pattern, &from, &to);
                    rename_uses_block(body, &from, &to);
                }
            }
            rewrite_block(body, skip, counter);
        }
        ExprKind::For { iterable, body, .. } => {
            rewrite_expr(iterable, skip, counter);
            rewrite_block(body, skip, counter);
        }
        _ => crate::import_alias::walk_expr_children(
            e,
            &mut |c| rewrite_expr(c, skip, counter),
            &mut |b| rewrite_block(b, skip, counter),
        ),
    }
}
