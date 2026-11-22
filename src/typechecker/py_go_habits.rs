//! Python's first-day habits that only the checker can mend, because the
//! Kāra form depends on a type (B-2026-11-20-73, -76): `x in v` (`contains`
//! or `contains_key`), `d.get(k, default)`, `d.items()`, `max(xs)` /
//! `min(xs)` and `print(a, b)`.
//!
//! Each is refused with its Kāra spelling; the edit is offered only where
//! the rewrite keeps the program's meaning.

use super::inference::resolve_type_var_top;
use super::types::Type;
use super::{FixIt, TypeChecker, TypeErrorKind};
use crate::ast::{CallArg, Expr, ExprKind};
use crate::formatter::render_expr_one_line;
use crate::token::Span;

/// The method name the parser gives Python's `x in v` (and `x not in v`):
/// never a name a program can write, and always refused here.
pub(crate) const PYTHON_IN: &str = "__python_in__";
pub(crate) const PYTHON_NOT_IN: &str = "__python_not_in__";

/// A receiver whose text can take a `.method(..)` as written.
fn plain_receiver(e: &Expr) -> bool {
    matches!(
        e.kind,
        ExprKind::Identifier(_)
            | ExprKind::FieldAccess { .. }
            | ExprKind::Index { .. }
            | ExprKind::Call { .. }
            | ExprKind::MethodCall { .. }
            | ExprKind::ArrayLiteral(..)
            | ExprKind::StringLit(_)
    )
}

impl TypeChecker<'_> {
    fn habit_receiver_name(&self, ty: &Type) -> Option<String> {
        let ty = resolve_type_var_top(ty, &self.env.substitutions);
        let ty = match ty {
            Type::Ref(inner) | Type::MutRef(inner) => *inner,
            t => t,
        };
        match ty {
            Type::Named { name, .. } => Some(name),
            Type::Str => Some("Str".to_string()),
            Type::Array { .. } => Some("Array".to_string()),
            Type::Slice { .. } => Some("Slice".to_string()),
            _ => None,
        }
    }

    /// `x in v` / `x not in v`, which the parser reads as a call of
    /// [`PYTHON_IN`] on `v`. Always an error; a `bool` either way.
    pub(super) fn check_python_membership(
        &mut self,
        object: &Expr,
        negated: bool,
        args: &[CallArg],
        span: &Span,
    ) -> Type {
        let recv = self.infer_expr(object);
        for a in args {
            self.infer_expr(&a.value);
        }
        let word = if negated { "not in" } else { "in" };
        let method = match self.habit_receiver_name(&recv).as_deref() {
            Some("Map" | "SortedMap") => Some("contains_key"),
            Some(
                "Vec" | "VecDeque" | "Set" | "SortedSet" | "Array" | "Slice" | "String" | "Str"
                | "StringSlice",
            ) => Some("contains"),
            _ => None,
        };
        let message = format!(
            "`{word}` is Python's membership test: Kāra asks the collection, \
             `v.contains(x)` (`m.contains_key(k)` on a `Map`){}",
            if negated {
                ", and negates it with `not`"
            } else {
                ""
            }
        );
        match (method, args.first()) {
            (Some(m), Some(item)) => {
                let recv_text = render_expr_one_line(object);
                let recv_text = if plain_receiver(object) {
                    recv_text
                } else {
                    format!("({recv_text})")
                };
                let replacement = format!(
                    "{}{recv_text}.{m}({})",
                    if negated { "not " } else { "" },
                    render_expr_one_line(&item.value)
                );
                self.type_error_with_fix_it(
                    message,
                    *span,
                    TypeErrorKind::NoMethodFound,
                    FixIt {
                        span: *span,
                        replacement,
                    },
                );
            }
            _ => self.type_error(message, *span, TypeErrorKind::NoMethodFound),
        }
        Type::Bool
    }

    /// `d.items()` and `d.get(k, default)` on a `Map` / `SortedMap`.
    /// `None` when the call is not one of them.
    pub(super) fn python_map_habit(
        &mut self,
        method: &str,
        args: &[CallArg],
        span: &Span,
        args_close_span: &Span,
        val: &Type,
    ) -> Option<Type> {
        match (method, args.len()) {
            ("items", 0) => {
                let message = "no method 'items' on a map: Python's `d.items()` is Kāra's \
                               `d.iter()`, which yields the `(key, value)` pairs \
                               (library/collections.md)"
                    .to_string();
                // `items()` written unspaced ends at the `)`.
                match args_close_span.offset.checked_sub(1 + method.len()) {
                    Some(offset) => self.type_error_with_fix_it(
                        message,
                        *span,
                        TypeErrorKind::NoMethodFound,
                        FixIt {
                            span: Span {
                                offset,
                                length: method.len(),
                                ..*args_close_span
                            },
                            replacement: "iter".to_string(),
                        },
                    ),
                    None => self.type_error(message, *span, TypeErrorKind::NoMethodFound),
                }
                Some(Type::Error)
            }
            ("get", 2) => {
                for a in args {
                    self.infer_expr(&a.value);
                }
                let copy = self.is_copy_type_during_check(val);
                let take = if copy { "" } else { ".cloned()" };
                let message = format!(
                    "a map's `get` takes the key alone and returns an `Option`; Python's \
                     `d.get(k, default)` is Kāra's `d.get(k){take} ?? default` \
                     (library/collections.md)"
                );
                // `get(` written unspaced: the name ends where the key starts.
                let start = args[0].value.span.offset.checked_sub(1 + method.len());
                match start {
                    Some(offset) => {
                        let end = args_close_span.offset + args_close_span.length;
                        // `unwrap_or` is what `??` stands for, and keeps its
                        // place in any expression the call sits in.
                        let replacement = format!(
                            "get({}){take}.unwrap_or({})",
                            render_expr_one_line(&args[0].value),
                            render_expr_one_line(&args[1].value)
                        );
                        self.type_error_with_fix_it(
                            message,
                            *span,
                            TypeErrorKind::WrongNumberOfArgs,
                            FixIt {
                                span: Span {
                                    offset,
                                    length: end.saturating_sub(offset),
                                    ..*args_close_span
                                },
                                replacement,
                            },
                        )
                    }
                    None => self.type_error(message, *span, TypeErrorKind::WrongNumberOfArgs),
                }
                Some(Type::Error)
            }
            _ => None,
        }
    }

    /// `print(a, b)` (Python prints each, a space between, and a newline)
    /// and `max(xs)` / `min(xs)` of one collection. `None` when the call is
    /// neither.
    pub(super) fn python_call_habit(
        &mut self,
        callee: &Expr,
        args: &[CallArg],
        span: &Span,
    ) -> Option<Type> {
        let ExprKind::Identifier(name) = &callee.kind else {
            return None;
        };
        if self.local_scope.lookup(name).is_some() {
            return None;
        }
        match name.as_str() {
            "print" | "println" if args.len() >= 2 => {
                for a in args {
                    self.infer_expr(&a.value);
                }
                let message = format!(
                    "`{name}` takes one value: Python's `print(a, b)` is Kāra's \
                     `println(f\"{{a}} {{b}}\")`, one f-string with a space between the \
                     values"
                );
                let mut parts = Vec::new();
                for a in args {
                    let part = match &a.value.kind {
                        ExprKind::StringLit(s) if !s.contains(['\\', '"', '{', '}', '\n']) => {
                            Some(s.clone())
                        }
                        _ => {
                            let t = render_expr_one_line(&a.value);
                            (!t.contains(['"', '{', '}', '\\']) && a.label.is_none())
                                .then(|| format!("{{{t}}}"))
                        }
                    };
                    match part {
                        Some(p) => parts.push(p),
                        None => {
                            self.type_error(message, *span, TypeErrorKind::WrongNumberOfArgs);
                            return Some(Type::Unit);
                        }
                    }
                }
                // Python's `print` ends the line; `print` in Kāra does not.
                self.type_error_with_fix_it(
                    message,
                    *span,
                    TypeErrorKind::WrongNumberOfArgs,
                    FixIt {
                        span: *span,
                        replacement: format!("println(f\"{}\")", parts.join(" ")),
                    },
                );
                Some(Type::Unit)
            }
            "max" | "min"
                if args.len() == 1
                    && self
                        .env
                        .functions
                        .get(name.as_str())
                        .is_none_or(|f| f.params.len() == 2) =>
            {
                self.infer_expr(&args[0].value);
                let message = format!(
                    "`{name}` compares two values, `{name}(a, b)` or `a.{name}(b)`; the \
                     {} item of a collection is `xs.iter().{name}()`, an `Option` that is \
                     `None` when the collection is empty, so `.unwrap()` panics there as \
                     Python's `{name}([])` raises (library/iterators.md)",
                    if name == "max" { "largest" } else { "smallest" }
                );
                let arg = &args[0].value;
                let text = render_expr_one_line(arg);
                let text = if plain_receiver(arg) {
                    text
                } else {
                    format!("({text})")
                };
                self.type_error_with_fix_it(
                    message,
                    *span,
                    TypeErrorKind::WrongNumberOfArgs,
                    FixIt {
                        span: *span,
                        replacement: format!("{text}.iter().{name}().unwrap()"),
                    },
                );
                Some(Type::Error)
            }
            _ => None,
        }
    }

    /// Python's `s.split()` (on whitespace) is Kāra's `s.split_whitespace()`:
    /// Kāra's own `split` takes a separator. `None` when `object` is not text.
    pub(super) fn python_split_without_separator(
        &mut self,
        object: &Expr,
        args_close_span: &Span,
    ) -> Option<Type> {
        if let ExprKind::Identifier(n) = &object.kind {
            self.local_scope.lookup(n)?;
        }
        let recv = self.infer_expr(object);
        if !matches!(
            self.habit_receiver_name(&recv).as_deref(),
            Some("String" | "Str" | "StringSlice")
        ) {
            return None;
        }
        let message = "Kāra's `split` takes a separator, `s.split(\",\")`; Python's \
                       `s.split()` splits on whitespace, Kāra's `s.split_whitespace()` \
                       (library/strings.md)"
            .to_string();
        let span = object.span;
        match args_close_span.offset.checked_sub(1 + "split".len()) {
            Some(offset) if offset > object.span.offset => self.type_error_with_fix_it(
                message,
                span,
                TypeErrorKind::WrongNumberOfArgs,
                FixIt {
                    span: Span {
                        offset,
                        length: "split".len(),
                        ..*args_close_span
                    },
                    replacement: "split_whitespace".to_string(),
                },
            ),
            _ => self.type_error(message, span, TypeErrorKind::WrongNumberOfArgs),
        }
        Some(Type::Error)
    }
}
