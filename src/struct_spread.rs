//! Struct functional update — `P { x: 1, ..base }` (B-2026-08-21-18).
//!
//! Rewrites a spread base into the explicit field copies it stands for, before
//! the resolver runs:
//!
//! ```text
//! P { x: 1, ..base }   ==>   P { x: 1, y: base.y, z: base.z }
//! ```
//!
//! WHY A DESUGAR AND NOT A CODEGEN ARM. The parser already stores the base and
//! the interpreter already implements the copy, so the missing piece was
//! codegen — and the row that tracked this said the hard part there is not
//! emitting the copy but OWNERSHIP: who owns a heap field copied out of the
//! base, whether the base is moved or borrowed, what a copied RC field does to
//! the refcount, whether a partially-spread base may still be used. Those are
//! the questions behind a large share of this ledger's double-free and leak
//! rows.
//!
//! Rewriting to `y: base.y` answers none of them itself — it inherits the
//! answers, because that is a shape every phase already handles and every one
//! of those questions has already been settled for it. Measured before
//! writing this: the hand-written form type-checks and runs for `String` and
//! `Vec[T]` fields, and reusing the base afterwards is correctly reported as
//! `value 'b' moved here, used again here`. Getting the same behaviour by
//! construction is worth more than a second implementation that has to be
//! kept in agreement with it.
//!
//! It is also exactly what the pre-existing diagnostic told users to write by
//! hand ("copy the remaining fields explicitly instead: `y: <base>.y`"), so
//! the feature and the advice cannot disagree.
//!
//! THE BASE MUST BE RE-EVALUABLE. Each copied field re-evaluates the base
//! expression, so this only fires for a side-effect-free place: a plain
//! binding or `self`. `P { x: 1, ..make()! }` would otherwise call `make()`
//! once per copied field — a silent change in both effect count and cost. Any
//! other base keeps the spread node, and the typechecker's existing
//! unsupported-spread error still fires on it with a message naming the
//! restriction.

use crate::ast::{Expr, ExprKind, FieldInit, ImplItem, Item, Program, TraitItem};
use crate::token::Span;
use std::collections::HashMap;

/// Field names per struct, in declaration order.
type StructFields = HashMap<String, Vec<String>>;

pub fn expand_struct_spreads(program: &mut Program) {
    let mut fields: StructFields = HashMap::new();
    for item in &program.items {
        if let Item::StructDef(s) = item {
            fields.insert(
                s.name.clone(),
                s.fields.iter().map(|f| f.name.clone()).collect(),
            );
        }
    }
    if fields.is_empty() {
        return;
    }
    // `take`/restore so the walker can borrow `program.items` immutably for
    // nothing else — the field map is already owned above.
    let mut items = std::mem::take(&mut program.items);
    for item in &mut items {
        match item {
            Item::Function(f) => walk_block(&mut f.body, &fields),
            Item::ImplBlock(i) => {
                for it in &mut i.items {
                    if let ImplItem::Method(m) = it {
                        walk_block(&mut m.body, &fields);
                    }
                }
            }
            Item::TraitDef(t) => {
                for it in &mut t.items {
                    if let TraitItem::Method(m) = it {
                        if let Some(b) = &mut m.body {
                            walk_block(b, &fields);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    program.items = items;
}

/// True for a base expression cheap and safe to evaluate once per copied
/// field. Deliberately narrow — see the module note.
fn base_is_reevaluable(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Identifier(_) | ExprKind::SelfValue => true,
        // A field path off a re-evaluable root is itself a place with no side
        // effects (`..cfg.defaults`).
        ExprKind::FieldAccess { object, .. } => base_is_reevaluable(object),
        _ => false,
    }
}

fn rewrite_struct_literal(e: &mut Expr, structs: &StructFields) {
    let ExprKind::StructLiteral {
        path,
        fields,
        spread,
    } = &mut e.kind
    else {
        return;
    };
    let Some(base) = spread.as_ref() else {
        return;
    };
    if !base_is_reevaluable(base) {
        return;
    }
    let Some(name) = path.last() else {
        return;
    };
    let Some(declared) = structs.get(name.as_str()) else {
        // Not a known struct here — an enum struct-variant, or a type this
        // program does not declare. Leave it; the later phases diagnose.
        return;
    };
    let given: Vec<&str> = fields.iter().map(|f| f.name.as_str()).collect();
    let missing: Vec<String> = declared
        .iter()
        .filter(|d| !given.contains(&d.as_str()))
        .cloned()
        .collect();
    // A base that copies NOTHING is left in place on purpose. The typechecker
    // already reports it ("every field is already given explicitly here, so
    // the base has no effect — drop it"), and expanding it to zero fields
    // would silently accept a base the writer plainly did not mean to be
    // inert.
    if missing.is_empty() {
        return;
    }
    let base = base.clone();
    for (i, fname) in missing.into_iter().enumerate() {
        // DISTINCT synthetic spans, one per copied field — the crate's
        // `offset + i`, `length: 0` convention (`collect_synth_span`).
        //
        // Giving them all the base's own span instead is what the first
        // attempt did, and it is wrong in a way worth recording: move
        // tracking is SPAN-KEYED, so two heap fields copied from one base
        // read as the same use site twice and the ownership pass reported
        // `value 'b' moved here, used again here` for a functional update
        // that is perfectly legal. Measured against the hand-written
        // `P { x: 9, s: b.s, v: b.v }`, which passes clean — the desugar has
        // to be indistinguishable from it, and with one shared span it was
        // not.
        let span = Span {
            line: base.span.line,
            column: base.span.column,
            offset: base.span.offset + i + 1,
            length: 0,
        };
        fields.push(FieldInit {
            name: fname.clone(),
            value: Expr {
                kind: ExprKind::FieldAccess {
                    object: Box::new(Expr {
                        kind: base.kind.clone(),
                        span,
                    }),
                    field: fname,
                },
                span,
            },
            shorthand: false,
            span,
        });
    }
    // Consumed: every later phase now sees an ordinary complete literal, and
    // the typechecker's unsupported-spread error correctly does not fire.
    *spread = None;
}

// ── Traversal ───────────────────────────────────────────────────────
//
// Hand-rolled because the crate has no generic mutable expression walker and
// each existing pass carries its own. Structural and total-by-fallthrough: an
// unhandled variant contributes no sub-expressions, which here means a spread
// nested inside it keeps its base and is diagnosed by the typechecker exactly
// as before — the conservative direction.

use crate::ast::{Block, Stmt, StmtKind};

fn walk_block(b: &mut Block, s: &StructFields) {
    for st in &mut b.stmts {
        walk_stmt(st, s);
    }
    if let Some(t) = &mut b.final_expr {
        walk_expr(t, s);
    }
}

fn walk_stmt(st: &mut Stmt, s: &StructFields) {
    match &mut st.kind {
        StmtKind::Let { value, .. } | StmtKind::Expr(value) => walk_expr(value, s),
        StmtKind::LetElse {
            value, else_block, ..
        } => {
            walk_expr(value, s);
            walk_block(else_block, s);
        }
        StmtKind::Assign { target, value } => {
            walk_expr(target, s);
            walk_expr(value, s);
        }
        StmtKind::CompoundAssign { target, value, .. } => {
            walk_expr(target, s);
            walk_expr(value, s);
        }
        StmtKind::MultiAssign { targets, values } => {
            for t in targets {
                walk_expr(t, s);
            }
            for v in values {
                walk_expr(v, s);
            }
        }
        StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => walk_block(body, s),
        StmtKind::LetUninit { .. } => {}
    }
}

fn walk_expr(e: &mut Expr, s: &StructFields) {
    // Rewrite OUTERMOST-first so a spread whose base is itself a struct
    // literal with a spread still has its own base expanded by the recursion
    // below (the clone happens before the children are walked, so walk the
    // rewritten node's children afterwards).
    rewrite_struct_literal(e, s);
    match &mut e.kind {
        ExprKind::StructLiteral { fields, spread, .. } => {
            for f in fields {
                walk_expr(&mut f.value, s);
            }
            if let Some(b) = spread {
                walk_expr(b, s);
            }
        }
        ExprKind::Call { callee, args, .. } => {
            walk_expr(callee, s);
            for a in args {
                walk_expr(&mut a.value, s);
            }
        }
        ExprKind::MethodCall { object, args, .. } => {
            walk_expr(object, s);
            for a in args {
                walk_expr(&mut a.value, s);
            }
        }
        ExprKind::Binary { left, right, .. } => {
            walk_expr(left, s);
            walk_expr(right, s);
        }
        ExprKind::Unary { operand, .. } => walk_expr(operand, s),
        ExprKind::Question(i) => walk_expr(i, s),
        ExprKind::FieldAccess { object, .. } => walk_expr(object, s),
        ExprKind::Index { object, index } => {
            walk_expr(object, s);
            walk_expr(index, s);
        }
        ExprKind::Cast { expr, .. } => walk_expr(expr, s),
        ExprKind::Tuple(items)
        | ExprKind::ArrayLiteral(items)
        | ExprKind::PrefixCollectionLiteral { items, .. } => {
            for i in items {
                walk_expr(i, s);
            }
        }
        ExprKind::While {
            condition, body, ..
        } => {
            walk_expr(condition, s);
            walk_block(body, s);
        }
        ExprKind::WhileLet { value, body, .. } => {
            walk_expr(value, s);
            walk_block(body, s);
        }
        ExprKind::For { iterable, body, .. } => {
            walk_expr(iterable, s);
            walk_block(body, s);
        }
        ExprKind::Loop { body, .. } | ExprKind::LabeledBlock { body, .. } => walk_block(body, s),
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            walk_expr(condition, s);
            walk_block(then_block, s);
            if let Some(eb) = else_branch {
                walk_expr(eb, s);
            }
        }
        ExprKind::IfLet {
            value,
            then_block,
            else_branch,
            ..
        } => {
            walk_expr(value, s);
            walk_block(then_block, s);
            if let Some(eb) = else_branch {
                walk_expr(eb, s);
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            walk_expr(scrutinee, s);
            for a in arms {
                if let Some(g) = &mut a.guard {
                    walk_expr(g, s);
                }
                walk_expr(&mut a.body, s);
            }
        }
        ExprKind::Block(b)
        | ExprKind::Comptime(b)
        | ExprKind::Unsafe(b)
        | ExprKind::Try(b)
        | ExprKind::Seq(b)
        | ExprKind::Par(b) => walk_block(b, s),
        ExprKind::Lock { mutex, body, .. } => {
            walk_expr(mutex, s);
            walk_block(body, s);
        }
        ExprKind::Providers { bindings, body } => {
            for b in bindings {
                walk_expr(&mut b.value, s);
            }
            walk_block(body, s);
        }
        ExprKind::Range { start, end, .. } => {
            if let Some(x) = start {
                walk_expr(x, s);
            }
            if let Some(x) = end {
                walk_expr(x, s);
            }
        }
        ExprKind::Closure { body, .. } => walk_expr(body, s),
        _ => {}
    }
}
