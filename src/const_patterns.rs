//! A named constant in a pattern compares against its value; it never binds
//! a new name (design.md § Pattern matching, "`42`, `'a'`, `true`, a `const`
//! | That value"; B-2026-11-06-16).
//!
//! The parser cannot tell `LIMIT` the constant from `limit` the binding, and
//! every later phase read a bare name in a pattern as a binding, so
//! `match x { LIMIT => .. }` took the arm for any `x`. This pre-resolve pass
//! rewrites each refutable pattern (a `match` arm, `if let`, `while let`,
//! `let ... else`) in terms every phase already handles:
//!
//! - a constant of an integer or `char` type, and an integer type's limit
//!   (`i64.MAX`), becomes the one-value range `C..=C`, whose bounds the
//!   typechecker, the exhaustiveness check and the MIR builder evaluate;
//! - any other constant with a literal value (a `Str`, a `bool`) becomes
//!   that literal;
//! - any other constant becomes the same range, which the typechecker refuses
//!   as a bound that is no integer or `char` constant, rather than binding.
//!
//! Only names of the constant class (`SCREAMING_CASE`) are constants in a
//! pattern, so a lower-case binding is never rewritten.

use crate::ast::{
    Expr, ExprKind, Item, LiteralPattern, Pattern, PatternKind, Program, RangeBound, Stmt,
    StmtKind, TypeKind, UnaryOp,
};
use crate::span_visitor::{walk_item_mut_with, MutVisitor};
use crate::token::Span;
use std::collections::HashMap;

const INT_TYPES: &[&str] = &[
    "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize",
];

enum Shape {
    /// An integer or `char` constant: compared as `C..=C`.
    Ordinal,
    /// Any other constant whose value is a literal.
    Literal(LiteralPattern),
    /// Neither: rewritten as `C..=C` so the typechecker refuses it.
    Other,
}

fn shape(ty: &crate::ast::TypeExpr, value: &Expr) -> Shape {
    if let TypeKind::Path(p) = &ty.kind {
        if let [name] = p.segments.as_slice() {
            if INT_TYPES.contains(&name.as_str()) || name == "char" {
                return Shape::Ordinal;
            }
        }
    }
    let lit = match &value.kind {
        ExprKind::StringLit(s) => Some(LiteralPattern::String(s.clone())),
        ExprKind::Bool(b) => Some(LiteralPattern::Bool(*b)),
        ExprKind::Float(f, sfx) => Some(LiteralPattern::Float(*f, *sfx)),
        ExprKind::Unary {
            op: UnaryOp::Neg,
            operand,
        } => match &operand.kind {
            ExprKind::Float(f, sfx) => Some(LiteralPattern::Float(-*f, *sfx)),
            _ => None,
        },
        _ => None,
    };
    lit.map_or(Shape::Other, Shape::Literal)
}

/// `SCREAMING_CASE`: at least one letter and no lower-case one.
fn is_const_class(name: &str) -> bool {
    name.chars().any(|c| c.is_ascii_alphabetic())
        && !name.chars().any(|c| c.is_lowercase())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn one_value_range(segments: Vec<String>, span: Span) -> PatternKind {
    let bound = || {
        Some(RangeBound::Path {
            segments: segments.clone(),
            span,
        })
    };
    PatternKind::RangePattern {
        start: bound(),
        end: bound(),
        inclusive: true,
    }
}

struct Rewrite {
    consts: HashMap<String, Shape>,
}

impl Rewrite {
    fn rewrite(&self, p: &mut Pattern) {
        let span = p.span;
        match &mut p.kind {
            PatternKind::Binding(name) if !name.contains('.') => {
                if !is_const_class(name) {
                    return;
                }
                match self.consts.get(name.as_str()) {
                    Some(Shape::Literal(l)) => p.kind = PatternKind::Literal(l.clone()),
                    Some(Shape::Ordinal | Shape::Other) => {
                        p.kind = one_value_range(vec![name.clone()], span)
                    }
                    None => {}
                }
            }
            PatternKind::Binding(name) => {
                let segments: Vec<String> = name.split('.').map(str::to_string).collect();
                if let [ty, limit] = segments.as_slice() {
                    if INT_TYPES.contains(&ty.as_str()) && matches!(limit.as_str(), "MAX" | "MIN") {
                        p.kind = one_value_range(segments, span);
                    }
                }
            }
            PatternKind::AtBinding { pattern, .. } => self.rewrite(pattern),
            PatternKind::Struct { fields, .. } => {
                for f in fields {
                    if let Some(sub) = &mut f.pattern {
                        self.rewrite(sub);
                    }
                }
            }
            PatternKind::TupleVariant { patterns, .. }
            | PatternKind::Tuple(patterns)
            | PatternKind::Or(patterns) => {
                for sub in patterns {
                    self.rewrite(sub);
                }
            }
            PatternKind::Slice { prefix, suffix, .. } => {
                for sub in prefix.iter_mut().chain(suffix.iter_mut()) {
                    self.rewrite(sub);
                }
            }
            PatternKind::Wildcard | PatternKind::Literal(_) | PatternKind::RangePattern { .. } => {}
        }
    }
}

impl MutVisitor for Rewrite {
    fn span(&mut self, _span: &mut Span) {}

    fn expr(&mut self, e: &mut Expr) {
        match &mut e.kind {
            ExprKind::IfLet { pattern, .. } | ExprKind::WhileLet { pattern, .. } => {
                self.rewrite(pattern)
            }
            ExprKind::Match { arms, .. } => {
                for arm in arms {
                    self.rewrite(&mut arm.pattern);
                }
            }
            _ => {}
        }
    }

    fn stmt(&mut self, s: &mut Stmt) {
        if let StmtKind::LetElse { pattern, .. } = &mut s.kind {
            self.rewrite(pattern);
        }
    }
}

/// Rewrite every named constant in a refutable pattern of `program` into the
/// value it names. See the module note.
pub fn rewrite_const_patterns(program: &mut Program) {
    let consts: HashMap<String, Shape> = program
        .items
        .iter()
        .filter_map(|item| match item {
            Item::ConstDecl(c) => Some((c.name.clone(), shape(&c.ty, &c.value))),
            _ => None,
        })
        .collect();
    let mut rewrite = Rewrite { consts };
    for item in program.items.iter_mut() {
        walk_item_mut_with(item, &mut rewrite);
    }
}
