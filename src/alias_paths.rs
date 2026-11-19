//! A type alias of a struct names that struct (design.md § Type aliases), so
//! it builds and matches it as the struct's own name does (B-2026-11-19-8):
//! `type Q = P; Q { a: 1 }` and the pattern `Q { a }` are `P { a: 1 }` and
//! `P { a }`, and `type B = Boxed[i64]; B { v: 1 }` is `Boxed[i64] { v: 1 }`.
//! Enum aliases already resolve through the type checker's canonical names;
//! a struct literal and a struct pattern are looked up by the path written,
//! so this pass rewrites the path. The formatter skips desugaring, so
//! `karac fmt` keeps the alias as written.

use crate::ast::{Expr, ExprKind, GenericArg, Item, Pattern, PatternKind, Program, TypeKind};
use crate::span_visitor::{walk_item_mut_with, MutVisitor};
use crate::token::Span;
use std::collections::{HashMap, HashSet};

pub fn desugar_alias_paths(program: &mut Program) {
    let structs: HashSet<String> = program
        .items
        .iter()
        .filter_map(|it| match it {
            Item::StructDef(s) => Some(s.name.clone()),
            _ => None,
        })
        .collect();
    // A plain (non-generic, unrefined) alias whose type is a one-segment path.
    let mut direct: HashMap<String, (String, Option<Vec<GenericArg>>)> = HashMap::new();
    for it in &program.items {
        let Item::TypeAlias(a) = it else { continue };
        if a.generic_params.is_some() || a.refinement.is_some() || structs.contains(&a.name) {
            continue;
        }
        if let TypeKind::Path(p) = &a.ty.kind {
            if p.segments.len() == 1 {
                direct.insert(
                    a.name.clone(),
                    (p.segments[0].clone(), p.generic_args.clone()),
                );
            }
        }
    }
    // Follow alias chains to a struct; a cycle or a non-struct end is left
    // alone for the checker to report.
    let mut targets = HashMap::new();
    for name in direct.keys() {
        let (mut at, mut args) = direct[name].clone();
        let mut hops = 0;
        while let Some((next, next_args)) = direct.get(&at) {
            if hops > 16 {
                break;
            }
            hops += 1;
            if args.is_none() {
                args = next_args.clone();
            }
            at = next.clone();
        }
        if structs.contains(&at) {
            targets.insert(name.clone(), (at, args));
        }
    }
    if targets.is_empty() {
        return;
    }
    let mut pass = Pass { targets };
    for item in &mut program.items {
        walk_item_mut_with(item, &mut pass);
    }
}

struct Pass {
    targets: HashMap<String, (String, Option<Vec<GenericArg>>)>,
}

impl MutVisitor for Pass {
    fn span(&mut self, _: &mut Span) {}

    fn expr(&mut self, e: &mut Expr) {
        if let ExprKind::StructLiteral {
            path, generic_args, ..
        } = &mut e.kind
        {
            if let [name] = path.as_slice() {
                if let Some((to, args)) = self.targets.get(name) {
                    *path = vec![to.clone()];
                    if generic_args.is_none() {
                        *generic_args = args.clone();
                    }
                }
            }
        }
    }

    fn pattern(&mut self, p: &mut Pattern) {
        if let PatternKind::Struct { path, .. } = &mut p.kind {
            if let [name] = path.as_slice() {
                if let Some((to, _)) = self.targets.get(name) {
                    *path = vec![to.clone()];
                }
            }
        }
    }
}
