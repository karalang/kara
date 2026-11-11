//! Rust's `scan` closure, carried over (B-2026-11-08-45).
//!
//! Rust's `Iterator::scan` lends its closure the state, `|m, x| { *m =
//! (*m).max(x); Some(*m) }`, and the closure writes it in place. Kāra's
//! takes the state by value and returns it with the item,
//! `Some((next_state, item))` (`runtime/stdlib/source/iter.kara`). The
//! Rust body drew a "drop the `*`" error per deref, whose fixes left a
//! closure returning `Option[i64]` where `Option[(i64, _)]` is wanted.
//!
//! When a failed `scan` call's closure has Rust's shape (its state named
//! by a plain parameter that the body only ever dereferences, and a tail
//! `Some(item)`), its errors are replaced by one that says what Kāra's
//! `scan` takes, and, when every write of the state is a statement of the
//! body's own block, carries the rewrite: each `*m = e` becomes `let m =
//! e`, each `*m op= e` `let m = m op e`, every other `*m` reads `m`, and
//! the tail returns `Some((m, item))`. A `return None` stays as it is:
//! it ends the iteration in either shape (kata 1103).

use crate::ast::{
    BinOp, Block, CallArg, CompoundOp, Expr, ExprKind, Pattern, PatternKind, StmtKind, UnaryOp,
};
use crate::resolver::SpanKey;
use crate::span_visitor::MutVisitor;
use crate::token::Span;

use super::{TypeChecker, TypeErrorKind};

/// A closure written over Rust's lent state: the state's name, and the
/// rewritten closure when every write of the state is mechanical.
struct RustScan {
    state: String,
    rewrite: Option<Expr>,
}

fn is_deref_of(e: &Expr, name: &str) -> bool {
    matches!(&e.kind, ExprKind::Unary { op: UnaryOp::Deref, operand }
        if matches!(&operand.kind, ExprKind::Identifier(n) if n == name))
}

/// How `name` is used in `e`: (dereferenced at least once, used bare).
/// A field or tuple-index read, `m.0`, is not a bare use: Rust reaches
/// the lent state's fields through the reference, and after the rewrite
/// it reads the state's newest binding just as it read the state's
/// newest value (kata 2104's `Some(bounds.1 - bounds.0)`).
fn uses(e: &Expr, name: &str) -> (bool, bool) {
    let mut deref = false;
    let mut reads: Vec<Span> = Vec::new();
    let mut bare: Vec<Span> = Vec::new();
    crate::span_visitor::for_each_expr(e, &mut |x| {
        if is_deref_of(x, name) {
            deref = true;
            if let ExprKind::Unary { operand, .. } = &x.kind {
                reads.push(operand.span);
            }
        } else if let ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } =
            &x.kind
        {
            if matches!(&object.kind, ExprKind::Identifier(n) if n == name) {
                reads.push(object.span);
            }
        } else if matches!(&x.kind, ExprKind::Identifier(n) if n == name) {
            bare.push(x.span);
        }
    });
    (deref, bare.iter().any(|s| !reads.contains(s)))
}

/// Whether a statement of `body` writes a field of `name`, `m.0 = e`:
/// a write through the lent state that a `let` cannot carry.
fn writes_field_of(body: &Expr, name: &str) -> bool {
    let ExprKind::Block(b) = &body.kind else {
        return false;
    };
    let mut found = false;
    crate::span_visitor::for_each_stmt_in_block(b, &mut |s| {
        if let StmtKind::Assign { target, .. } | StmtKind::CompoundAssign { target, .. } = &s.kind {
            let mut t = target;
            while let ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } =
                &t.kind
            {
                t = object;
            }
            found |=
                !std::ptr::eq(t, target) && matches!(&t.kind, ExprKind::Identifier(n) if n == name);
        }
    });
    found
}

/// `Some(item)` with `item` not already a pair.
fn some_item(e: &Expr) -> bool {
    matches!(&e.kind, ExprKind::Call { callee, args }
        if matches!(&callee.kind, ExprKind::Identifier(n) if n == "Some")
            && args.len() == 1
            && !matches!(&args[0].value.kind, ExprKind::Tuple(t) if t.len() == 2))
}

fn compound_binop(op: &CompoundOp) -> BinOp {
    super::TypeChecker::compound_op_binop(op)
}

/// Every `*name` read as `name`.
struct Underef<'a>(&'a str);

impl MutVisitor for Underef<'_> {
    fn span(&mut self, _: &mut Span) {}
    fn expr(&mut self, e: &mut Expr) {
        if is_deref_of(e, self.0) {
            if let ExprKind::Unary { operand, .. } = &e.kind {
                *e = (**operand).clone();
            }
        }
    }
}

fn binding(name: &str, span: Span) -> Pattern {
    Pattern {
        kind: PatternKind::Binding(name.to_string()),
        span,
        id: crate::ids::NodeId::DUMMY,
    }
}

/// How far a rewritten closure's later lines are indented: one level out
/// from its body's first statement when that starts a line of its own,
/// else not at all.
fn continuation_indent(closure: &Expr) -> usize {
    let ExprKind::Closure { body, .. } = &closure.kind else {
        return 0;
    };
    let ExprKind::Block(b) = &body.kind else {
        return 0;
    };
    let first = match (b.stmts.first(), b.final_expr.as_deref()) {
        (Some(s), _) => s.span,
        (None, Some(e)) => e.span,
        (None, None) => return 0,
    };
    if first.line == closure.span.line {
        return 0;
    }
    first.column.saturating_sub(1).saturating_sub(4)
}

fn rust_scan(closure: &Expr) -> Option<RustScan> {
    let ExprKind::Closure { params, body, .. } = &closure.kind else {
        return None;
    };
    let [state, _] = params.as_slice() else {
        return None;
    };
    let PatternKind::Binding(name) = &state.pattern.kind else {
        return None;
    };
    let (deref, bare) = uses(body, name);
    if !deref || bare || writes_field_of(body, name) {
        return None;
    }
    let tail = match &body.kind {
        ExprKind::Block(b) => b.final_expr.as_deref()?,
        _ => body,
    };
    if !some_item(tail) {
        return None;
    }
    let mut found = RustScan {
        state: name.clone(),
        rewrite: None,
    };
    // A `return` or a write of the state below the block's own statements
    // has no mechanical rewrite. `return None` is the exception: it ends
    // the iteration and carries no state, so it reads the same in Kāra's
    // shape (kata 1103's guard, `if *left <= 0 { return None; }`).
    let mut nested_write = false;
    let top: Vec<Span> = match &body.kind {
        ExprKind::Block(b) => b.stmts.iter().map(|s| s.span).collect(),
        _ => Vec::new(),
    };
    let mut returns = false;
    crate::span_visitor::for_each_expr(body, &mut |x| {
        if let ExprKind::Return(value) = &x.kind {
            let none = matches!(value.as_deref(),
                Some(Expr { kind: ExprKind::Identifier(n), .. }) if n == "None");
            returns |= !none;
        }
    });
    if let ExprKind::Block(b) = &body.kind {
        crate::span_visitor::for_each_stmt_in_block(b, &mut |s| {
            let writes = match &s.kind {
                StmtKind::Assign { target, .. } | StmtKind::CompoundAssign { target, .. } => {
                    is_deref_of(target, name)
                }
                _ => false,
            };
            if writes && !top.contains(&s.span) {
                nested_write = true;
            }
        });
    }
    if returns || nested_write {
        return Some(found);
    }
    let mut new = closure.clone();
    let ExprKind::Closure { body: new_body, .. } = &mut new.kind else {
        return None;
    };
    let mut block = match &new_body.kind {
        ExprKind::Block(b) => b.clone(),
        _ => Block {
            stmts: Vec::new(),
            final_expr: Some(Box::new((**new_body).clone())),
            span: new_body.span,
        },
    };
    for s in &mut block.stmts {
        let rewritten = match &s.kind {
            StmtKind::Assign { target, value } if is_deref_of(target, name) => Some(value.clone()),
            StmtKind::CompoundAssign { target, op, value } if is_deref_of(target, name) => {
                Some(Expr {
                    kind: ExprKind::Binary {
                        op: compound_binop(op),
                        left: Box::new(Expr {
                            kind: ExprKind::Identifier(name.clone()),
                            span: target.span,
                            id: crate::ids::NodeId::DUMMY,
                        }),
                        right: Box::new(value.clone()),
                    },
                    span: s.span,
                    id: crate::ids::NodeId::DUMMY,
                })
            }
            _ => None,
        };
        if let Some(value) = rewritten {
            s.kind = StmtKind::Let {
                is_mut: false,
                pattern: binding(name, s.span),
                ty: None,
                value,
            };
        }
    }
    let mut tail = *block.final_expr.take()?;
    if let ExprKind::Call { args, .. } = &mut tail.kind {
        let item = args[0].value.clone();
        args[0] = CallArg {
            value: Expr {
                kind: ExprKind::Tuple(vec![
                    Expr {
                        kind: ExprKind::Identifier(name.clone()),
                        span: item.span,
                        id: crate::ids::NodeId::DUMMY,
                    },
                    item,
                ]),
                span: args[0].span,
                id: crate::ids::NodeId::DUMMY,
            },
            ..args[0].clone()
        };
    }
    block.final_expr = Some(Box::new(tail));
    let mut block_expr = Expr {
        kind: ExprKind::Block(block),
        span: new_body.span,
        id: crate::ids::NodeId::DUMMY,
    };
    crate::span_visitor::walk_expr_mut_with(&mut block_expr, &mut Underef(name));
    **new_body = block_expr;
    found.rewrite = Some(new);
    Some(found)
}

impl TypeChecker<'_> {
    /// After a `scan` call whose check failed inside its closure: when the
    /// closure is written over Rust's lent state, its errors give way to
    /// one naming Kāra's shape, with the rewrite when there is one.
    pub(super) fn explain_rust_scan(&mut self, args: &[CallArg], errors_before: usize) {
        let Some(closure) = args.get(1).map(|a| &a.value) else {
            return;
        };
        let within = |s: &Span| {
            s.offset >= closure.span.offset
                && s.offset + s.length <= closure.span.offset + closure.span.length
        };
        if !self.errors[errors_before..].iter().any(|e| within(&e.span)) {
            return;
        }
        let Some(found) = rust_scan(closure) else {
            return;
        };
        let mut i = errors_before;
        while i < self.errors.len() {
            if within(&self.errors[i].span) {
                let e = self.errors.remove(i);
                self.error_fix_diffs.remove(&SpanKey::from_span(&e.span));
            } else {
                i += 1;
            }
        }
        let name = &found.state;
        let shape = "Kāra's `scan` closure takes the state by value and returns \
                     `Some((next_state, item))`, where Rust's lends it to be written in place";
        let how =
            format!("bind each new state with `let {name} = ..` and return `Some(({name}, item))`");
        match found.rewrite {
            Some(new) => {
                let mut text = crate::formatter::render_expr(&new);
                // A short closure stays on its line.
                let one: Vec<&str> = text.lines().map(str::trim).collect();
                if !text.contains('"') && one.iter().map(|l| l.len() + 1).sum::<usize>() <= 100 {
                    text = one.join(" ");
                } else {
                    // A longer one keeps its body where it was: the later
                    // lines move out to one level outside the first
                    // statement's column (B-2026-11-10-32).
                    let pad = " ".repeat(continuation_indent(closure));
                    text = text
                        .lines()
                        .enumerate()
                        .map(|(i, l)| {
                            if i == 0 || l.is_empty() {
                                l.to_string()
                            } else {
                                format!("{pad}{l}")
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                }
                self.error_fix_diffs.insert(
                    SpanKey::from_span(&closure.span),
                    vec![crate::resolver::TextEdit {
                        offset: closure.span.offset,
                        length: closure.span.length,
                        replacement: text.clone(),
                    }],
                );
                let write = if text.contains('\n') {
                    format!("{how}, as `karac fix` writes it")
                } else {
                    format!("write `{text}`")
                };
                self.type_error(
                    format!(
                        "this closure writes the state `{name}` through `*{name}`, as Rust's \
                         `scan` does; {shape}: {write}"
                    ),
                    closure.span,
                    TypeErrorKind::TypeMismatch,
                );
            }
            None => self.type_error(
                format!(
                    "this closure writes the state `{name}` through `*{name}`, as Rust's \
                     `scan` does; {shape}: {how}"
                ),
                closure.span,
                TypeErrorKind::TypeMismatch,
            ),
        }
    }
}
