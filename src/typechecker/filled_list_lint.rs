//! `vec_per_element` (B-2026-11-19-102): `let seen = Vec.filled(n, [false,
//! false]);` is Rust's `vec![[false, false]; n]` written as it reads, but a
//! list literal is a `Vec` unless an `Array` is expected (design.md § List
//! literals), so `filled` clones a heap `Vec[bool]` into every slot and every
//! `seen[p][0]` reads through a second pointer. Kata 1654 ran 13x Rust until
//! it wrote `[false; 2]`, an inline `Array[bool, 2]`.
//!
//! The literal is not retyped silently: the lint names the cost and, when
//! every element is the same literal, `karac fix` writes `[v; N]`. It fires
//! only where the inline array is the same program: the binding is an
//! un-annotated `let` of a `Vec.filled` whose element is a non-empty list of
//! scalar literals, and every later mention of it in its block is an element
//! access `name[i][j]` (read or written) or a length (`name.len()`,
//! `name.is_empty()`, `name[i].len()`). Anything else, a push onto a row
//! included, keeps the `Vec`.

use crate::ast::*;
use crate::span_visitor::{walk_expr_mut_with, MutVisitor};

use super::{FixIt, Span, TypeErrorKind};

/// A scalar literal's value, to tell whether a list repeats one.
fn literal_key(e: &Expr) -> Option<String> {
    match &e.kind {
        ExprKind::Bool(b) => Some(format!("b{b}")),
        ExprKind::Integer(v, s) => Some(format!("i{v}{s:?}")),
        ExprKind::Float(v, s) => Some(format!("f{}{s:?}", v.to_bits())),
        ExprKind::CharLit(c) => Some(format!("c{c}")),
        ExprKind::Unary {
            op: UnaryOp::Neg,
            operand,
        } => literal_key(operand).map(|k| format!("-{k}")),
        // `i64.MAX`, `u8.MIN`: a primitive's bound (kata 1591's
        // `Vec.filled(61, [i64.MAX, 0, i64.MAX, 0])`).
        ExprKind::FieldAccess { object, field } if matches!(field.as_str(), "MAX" | "MIN") => {
            match &object.kind {
                ExprKind::Identifier(t) if is_primitive_number(t) => Some(format!("{t}.{field}")),
                _ => None,
            }
        }
        ExprKind::Path {
            segments,
            generic_args: None,
        } if segments.len() == 2
            && is_primitive_number(&segments[0])
            && matches!(segments[1].as_str(), "MAX" | "MIN") =>
        {
            Some(segments.join("."))
        }
        _ => None,
    }
}

fn is_primitive_number(t: &str) -> bool {
    matches!(
        t,
        "i8" | "i16"
            | "i32"
            | "i64"
            | "i128"
            | "isize"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "u128"
            | "usize"
            | "f32"
            | "f64"
    )
}

fn is_name(e: &Expr, name: &str) -> bool {
    matches!(&e.kind, ExprKind::Identifier(n) if n == name)
}

/// Rewrites every allowed use of `name` to something that does not mention
/// it, keeping the index expressions so a mention inside them still counts.
struct StripUses<'n> {
    name: &'n str,
}

impl MutVisitor for StripUses<'_> {
    fn span(&mut self, _: &mut Span) {}

    fn expr(&mut self, e: &mut Expr) {
        let name = self.name;
        let replacement = match &mut e.kind {
            // `name[i][j]`
            ExprKind::Index { object, index } => match &mut object.kind {
                ExprKind::Index {
                    object: root,
                    index: row,
                } if is_name(root, name) => Some(ExprKind::Tuple(vec![
                    std::mem::replace(&mut **row, unit()),
                    std::mem::replace(&mut **index, unit()),
                ])),
                _ => None,
            },
            // `name.len()`, `name.is_empty()`, `name[i].len()`
            ExprKind::MethodCall {
                object,
                method,
                args,
                ..
            } if args.is_empty() && matches!(method.as_str(), "len" | "is_empty") => {
                if is_name(object, name) {
                    Some(ExprKind::Tuple(Vec::new()))
                } else if method == "len" {
                    match &mut object.kind {
                        ExprKind::Index {
                            object: root,
                            index,
                        } if is_name(root, name) => Some(ExprKind::Tuple(vec![std::mem::replace(
                            &mut **index,
                            unit(),
                        )])),
                        _ => None,
                    }
                } else {
                    None
                }
            }
            _ => None,
        };
        if let Some(k) = replacement {
            e.kind = k;
        }
    }
}

fn unit() -> Expr {
    Expr {
        kind: ExprKind::Tuple(Vec::new()),
        span: Span {
            offset: 0,
            length: 0,
            line: 0,
            column: 0,
        },
        id: crate::ids::NodeId::DUMMY,
    }
}

/// Whether anything in `rest` / `tail` mentions `name` other than as an
/// element access or a length.
fn other_use(rest: &[Stmt], tail: Option<&Expr>, name: &str) -> bool {
    let mut scope = Expr {
        kind: ExprKind::Block(Block {
            stmts: rest.to_vec(),
            final_expr: tail.map(|t| Box::new(t.clone())),
            span: unit().span,
        }),
        ..unit()
    };
    walk_expr_mut_with(&mut scope, &mut StripUses { name });
    crate::deque_head::expr_mentions_name_deep(&scope, name)
}

impl<'a> super::TypeChecker<'a> {
    /// The `vec_per_element` lint over one block's `let`s (see the module
    /// documentation).
    pub(super) fn lint_filled_list_literals(&mut self, block: &Block) {
        if self.current_fn_stdlib_origin {
            return;
        }
        for (i, stmt) in block.stmts.iter().enumerate() {
            let StmtKind::Let {
                pattern,
                ty: None,
                value,
                ..
            } = &stmt.kind
            else {
                continue;
            };
            let PatternKind::Binding(name) = &pattern.kind else {
                continue;
            };
            let ExprKind::Call { callee, args } = &value.kind else {
                continue;
            };
            if args.len() != 2
                || !matches!(&callee.kind, ExprKind::Path { segments, generic_args: None }
                    if segments.len() == 2 && segments[0] == "Vec" && segments[1] == "filled")
            {
                continue;
            }
            let lit = &args[1].value;
            let ExprKind::ArrayLiteral(elems) = &lit.kind else {
                continue;
            };
            let Some(keys) = elems.iter().map(literal_key).collect::<Option<Vec<_>>>() else {
                continue;
            };
            if keys.is_empty()
                || other_use(&block.stmts[i + 1..], block.final_expr.as_deref(), name)
            {
                continue;
            }
            let n = elems.len();
            let first = &elems[0];
            let same = keys.iter().all(|k| *k == keys[0]);
            // `[v, v]` becomes `[v; 2]`: the text after the first element,
            // up to the closing `]`, is replaced.
            let close = lit.span.offset + lit.span.length - 1;
            let from = first.span.offset + first.span.length;
            let fix = (same && close >= from).then(|| FixIt {
                span: Span {
                    offset: from,
                    length: close - from,
                    line: first.span.line,
                    column: first.span.column + first.span.length,
                },
                replacement: format!("; {n}"),
            });
            let advice = if same {
                format!("write `[v; {n}]` for an inline `Array`")
            } else {
                format!("annotate `{name}` as a `Vec` of `Array[T, {n}]` for an inline `Array`")
            };
            self.type_lint_warning_with_fix(
                format!(
                    "`Vec.filled` with a list literal puts a separate heap `Vec` in every \
                     slot of `{name}`, since a list literal is a `Vec` unless an `Array` is \
                     expected (design.md § List literals); every element read goes through \
                     a second pointer. {name}'s rows are only indexed, so {advice}"
                ),
                lit.span,
                TypeErrorKind::StyleLint,
                "vec_per_element",
                fix,
            );
        }
    }
}
