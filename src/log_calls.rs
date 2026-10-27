//! `std.log`'s level calls with fields (docs/library/log.md § Fields).
//!
//! `debug`, `info`, `warn` and `error` take a message and then open
//! `key: value` fields, a parameter list no Kāra function can declare. After
//! the gated import splices them in, a call with fields is rewritten into
//! the call chain the library defines for it:
//!
//! ```text
//! info(msg, method: m, status: 200)
//!   => __log_begin(1, msg).field("method", m).field("status", 200).emit()
//! ```
//!
//! so the message and the values are ordinary arguments, evaluated in the
//! order written and borrowed, and every later phase sees calls it already
//! handles. `field` turns its value into a `Json` tree only when the call is
//! kept. A call whose labels break the rules (a reserved label, one used
//! twice, a field with no label) is left as written, and the checker
//! reports it with [`label_error`].

use std::collections::{HashMap, HashSet};

use crate::ast::{CallArg, Expr, ExprKind, Item, Program, StmtKind};
use crate::span_visitor::MutVisitor;
use crate::token::Span;

/// The names a record already carries, which no field may use.
pub const RESERVED_LABELS: &[&str] = &["level", "msg", "trace_id", "span_id", "task_id"];

/// The level functions the program holds, by the name it calls them: each
/// one's rank and the name of the function that starts its call chain (the
/// splice may have renamed it).
pub fn level_fns(program: &Program) -> HashMap<String, (i128, String)> {
    let mut out = HashMap::new();
    for item in &program.items {
        let Item::Function(f) = item else { continue };
        if !f.stdlib_origin
            || f.params.len() != 1
            || !f.attributes.iter().any(|a| a.is_bare("observability"))
        {
            continue;
        }
        // The body is `<begin>(<rank>, msg).emit();`.
        let call = match (f.body.stmts.as_slice(), &f.body.final_expr) {
            ([stmt], None) => match &stmt.kind {
                StmtKind::Expr(e) => e,
                _ => continue,
            },
            ([], Some(e)) => &**e,
            _ => continue,
        };
        let ExprKind::MethodCall {
            object,
            method,
            args,
            ..
        } = &call.kind
        else {
            continue;
        };
        if method != "emit" || !args.is_empty() {
            continue;
        }
        let ExprKind::Call { callee, args } = &object.kind else {
            continue;
        };
        let (ExprKind::Identifier(begin), [rank, _]) = (&callee.kind, args.as_slice()) else {
            continue;
        };
        let ExprKind::Integer(rank, _) = rank.value.kind else {
            continue;
        };
        if begin.starts_with("__log_begin") {
            out.insert(f.name.clone(), (rank, begin.clone()));
        }
    }
    out
}

/// What is wrong with the fields of a level call, and where, or `None` when
/// they follow the rules.
pub fn label_error(args: &[CallArg]) -> Option<(String, Span)> {
    let mut seen: HashSet<&str> = HashSet::new();
    for (i, arg) in args.iter().enumerate() {
        let Some(label) = arg.label.as_deref() else {
            if i == 0 {
                continue;
            }
            return Some((
                "a log field after the message is written `key: value`".to_string(),
                arg.span,
            ));
        };
        if i == 0 && label == "msg" {
            continue;
        }
        if RESERVED_LABELS.contains(&label) {
            return Some((
                format!(
                    "`{label}` cannot label a log field: every record already carries `{label}` \
                     (the reserved labels are `level`, `msg`, `trace_id`, `span_id` and `task_id`)"
                ),
                arg.span,
            ));
        }
        if !seen.insert(label) {
            return Some((
                format!("the log field `{label}` is given twice in one call"),
                arg.span,
            ));
        }
    }
    None
}

/// Rewrites every well-formed level call with fields in the program's own
/// code into its call chain.
pub fn rewrite_log_calls(program: &mut Program) {
    let fns = level_fns(program);
    if fns.is_empty() {
        return;
    }
    let mut rewrite = Rewrite(&fns);
    for item in &mut program.items {
        let library = match item {
            Item::Function(f) => f.stdlib_origin,
            _ => false,
        };
        if !library {
            crate::span_visitor::walk_item_mut_with(item, &mut rewrite);
        }
    }
}

struct Rewrite<'a>(&'a HashMap<String, (i128, String)>);

impl MutVisitor for Rewrite<'_> {
    fn span(&mut self, _: &mut Span) {}

    fn expr(&mut self, e: &mut Expr) {
        let ExprKind::Call { callee, args } = &e.kind else {
            return;
        };
        let ExprKind::Identifier(name) = &callee.kind else {
            return;
        };
        let Some((rank, begin)) = self.0.get(name) else {
            return;
        };
        if args.len() < 2 || label_error(args).is_some() {
            return;
        }
        let ExprKind::Call { callee, args } =
            std::mem::replace(&mut e.kind, ExprKind::Integer(0, None))
        else {
            unreachable!()
        };
        let at = |offset: usize, length: usize| Span {
            offset,
            length,
            ..callee.span
        };
        let expr = |kind: ExprKind, span: Span| Expr {
            kind,
            span,
            id: crate::ids::NodeId::DUMMY,
        };
        let arg = |value: Expr| CallArg {
            label: None,
            mut_marker: false,
            mut_marker_span: None,
            span: value.span,
            value,
        };
        let mut args = args.into_iter();
        let msg = args.next().unwrap();
        // The synthesized nodes take spans no other node has: the checker
        // keys what it learns about a call by its span.
        let start = callee.span.offset;
        let mut chain = expr(
            ExprKind::Call {
                callee: Box::new(expr(ExprKind::Identifier(begin.clone()), at(start, 0))),
                args: vec![
                    arg(expr(ExprKind::Integer(*rank, None), at(start + 1, 0))),
                    arg(msg.value),
                ],
            },
            callee.span,
        );
        for field in args {
            let label = field.label.unwrap();
            let key = expr(
                ExprKind::StringLit(label.clone()),
                Span {
                    length: label.len(),
                    ..field.span
                },
            );
            chain = expr(
                ExprKind::MethodCall {
                    object: Box::new(chain),
                    method: "field".to_string(),
                    turbofish: None,
                    args: vec![arg(key), arg(field.value)],
                    args_close_span: at(field.span.offset + field.span.length, 0),
                },
                field.span,
            );
        }
        *e = expr(
            ExprKind::MethodCall {
                object: Box::new(chain),
                method: "emit".to_string(),
                turbofish: None,
                args: vec![],
                args_close_span: at(e.span.offset + e.span.length, 0),
            },
            e.span,
        );
    }
}
