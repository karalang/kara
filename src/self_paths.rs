//! `Self` as the first segment of a path in an `impl` (B-2026-11-18-40):
//! `Self.gcd(a, b)`, `Self.B(3)`, `Self.MAX`, and the patterns `Self.A`,
//! `Self.B(x)` and `Self.C { .. }`. design.md's keyword table reads `Self`
//! as the type the `impl` is for, and this pass makes it that: each path
//! through `Self` in an `impl`'s methods becomes the path through the
//! type's name, with its type arguments (`Stack[T].new()` in `impl[T]
//! Stack[T]`), the same tree the parser builds for the name written out.
//!
//! It runs after the trait default methods are copied into each `impl`, so
//! a default body's `Self.one()` is the implementing type's `P.one()` in
//! each copy; the trait's own copy is checked with `Self` as the trait's
//! type parameter (`dispatch_self_receiver_method`). The formatter skips
//! desugaring, so `karac fmt` keeps `Self` as written.

use crate::ast::{
    CallArg, Expr, ExprKind, GenericArg, ImplItem, Item, Pattern, PatternKind, Program, TypeKind,
};
use crate::span_visitor::{walk_item_mut_with, MutVisitor};
use crate::token::Span;

pub fn desugar_self_paths(program: &mut Program) {
    for item in &mut program.items {
        let Item::ImplBlock(imp) = item else { continue };
        let TypeKind::Path(path) = &imp.target_type.kind else {
            continue;
        };
        let target = Target {
            segments: path.segments.clone(),
            args: path.generic_args.clone(),
        };
        let mut pass = Pass { target };
        for it in &mut imp.items {
            if let ImplItem::Method(f) = it {
                let mut wrapped = Item::Function((**f).clone());
                walk_item_mut_with(&mut wrapped, &mut pass);
                if let Item::Function(g) = wrapped {
                    **f = g;
                }
            }
        }
    }
}

struct Target {
    segments: Vec<String>,
    args: Option<Vec<GenericArg>>,
}

struct Pass {
    target: Target,
}

fn node(kind: ExprKind, span: Span) -> Expr {
    Expr {
        id: crate::ids::NodeId::DUMMY,
        span,
        kind,
    }
}

impl Pass {
    fn generic(&self) -> bool {
        self.target.args.as_ref().is_some_and(|a| !a.is_empty())
    }

    /// The type as a receiver, `Stack[T]` in `Stack[T].new()`: the path
    /// the parser builds for a type name with arguments before `.f(`.
    fn receiver(&self, span: Span) -> Expr {
        node(
            ExprKind::Path {
                segments: self.target.segments.clone(),
                generic_args: self.target.args.clone(),
            },
            span,
        )
    }

    fn path_through(&self, member: &str) -> Vec<String> {
        let mut segments = self.target.segments.clone();
        segments.push(member.to_string());
        segments
    }
}

impl MutVisitor for Pass {
    fn span(&mut self, _: &mut Span) {}

    fn expr(&mut self, e: &mut Expr) {
        match &mut e.kind {
            ExprKind::MethodCall {
                object,
                method,
                turbofish,
                args,
                ..
            } if matches!(object.kind, ExprKind::SelfType) => {
                // `Self.f(x)` with no type arguments to carry is `T.f(x)`,
                // which the parser reads as a call of the path `T.f`.
                if !self.generic() && turbofish.is_none() {
                    let callee = node(
                        ExprKind::Path {
                            segments: self.path_through(method),
                            generic_args: None,
                        },
                        e.span,
                    );
                    let args: Vec<CallArg> = std::mem::take(args);
                    e.kind = ExprKind::Call {
                        callee: Box::new(callee),
                        args,
                    };
                } else {
                    **object = self.receiver(object.span);
                }
            }
            // `Self.MAX`, `Self.Dot`: the two-segment path, with the type's
            // arguments where the parser attaches them in `Opt[T].No`.
            ExprKind::FieldAccess { object, field }
                if matches!(object.kind, ExprKind::SelfType) =>
            {
                e.kind = ExprKind::Path {
                    segments: self.path_through(field),
                    generic_args: if self.generic() {
                        self.target.args.clone()
                    } else {
                        None
                    },
                };
            }
            ExprKind::StructLiteral { path, .. }
                if path.first().is_some_and(|s| s == "Self") && path.len() > 1 =>
            {
                let mut segments = self.target.segments.clone();
                segments.extend(path.drain(1..));
                *path = segments;
            }
            _ => {}
        }
    }

    fn pattern(&mut self, p: &mut Pattern) {
        match &mut p.kind {
            PatternKind::Binding(name) => {
                if let Some(member) = name.strip_prefix("Self.") {
                    *name = self.path_through(member).join(".");
                }
            }
            PatternKind::TupleVariant { path, .. } | PatternKind::Struct { path, .. }
                if path.first().is_some_and(|s| s == "Self") && path.len() > 1 =>
            {
                let mut segments = self.target.segments.clone();
                segments.extend(path.drain(1..));
                *path = segments;
            }
            _ => {}
        }
    }
}
