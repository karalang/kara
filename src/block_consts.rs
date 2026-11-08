//! A block may declare a constant (design.md § Constants, B-2026-11-08-60):
//! `const MOD: i64 = 1_000_000_007;` as the first line of a function body,
//! the way a Rust solution writes it.
//!
//! The parser reads such a statement as the `let` it is written like and
//! marks it in [`Program::block_const_spans`] by its name's span, so no walk
//! in the compiler meets a new statement kind. This pass, the first of
//! desugaring, takes each marked `let` out of its block and declares it at
//! module scope under a name no source can collide with (`MOD__BLOCK1`),
//! then renames every use the declaration reaches: the rest of its block,
//! nested blocks included, up to a later declaration of the same name, which
//! shadows it as a `let` would. A use before the declaration, or outside the
//! block, keeps the source name, so it means whatever it meant without the
//! block's constant. From here on a block constant is an ordinary module
//! constant: its type and initializer get the module-scope checks, and each
//! use is a fresh value.
//!
//! The initializer may name earlier constants but no local binding; the
//! parser refuses a value-class name there, so what reaches this pass names
//! constants only.

use crate::ast::{Block, Expr, ExprKind, Pattern};
use crate::ast::{ConstDecl, Item, PatternKind, Program, RangeBound, Stmt, StmtKind};
use crate::resolver::SpanKey;
use crate::span_visitor::{walk_expr_mut_with, walk_item_mut_with, MutVisitor};
use crate::token::Span;
use rustc_hash::FxHashSet;

/// Where a constant-class name declared in a block means something else:
/// from the end of its declaration to the end of the block. `target` is the
/// hoisted constant's name, or `None` for a `let` that shadows one.
struct Region {
    name: String,
    start: usize,
    end: usize,
    target: Option<String>,
}

fn end(span: &Span) -> usize {
    span.offset + span.length
}

/// `SCREAMING_CASE`, the class a constant's name is in.
fn is_const_class(name: &str) -> bool {
    crate::lexer::classify_ident(name) == crate::lexer::IdentClass::Const
}

struct Found<'a> {
    marks: &'a FxHashSet<SpanKey>,
    taken: &'a mut FxHashSet<String>,
    regions: Vec<Region>,
    decls: Vec<ConstDecl>,
}

impl Found<'_> {
    fn fresh_name(&mut self, name: &str) -> String {
        let mut k = 1;
        loop {
            let candidate = format!("{name}__BLOCK{k}");
            if self.taken.insert(candidate.clone()) {
                return candidate;
            }
            k += 1;
        }
    }
}

impl MutVisitor for Found<'_> {
    fn span(&mut self, _: &mut Span) {}

    fn block(&mut self, b: &mut Block) {
        let block_end = end(&b.span);
        let mut hoisted = false;
        for s in b.stmts.iter() {
            let StmtKind::Let {
                pattern, ty, value, ..
            } = &s.kind
            else {
                continue;
            };
            let PatternKind::Binding(name) = &pattern.kind else {
                continue;
            };
            if !is_const_class(name) {
                continue;
            }
            let marked = self.marks.contains(&SpanKey::from_span(&pattern.span));
            let target = if marked {
                let fresh = self.fresh_name(name);
                if let Some(ty) = ty {
                    self.decls.push(ConstDecl {
                        span: s.span,
                        attributes: Vec::new(),
                        doc_comment: None,
                        is_pub: false,
                        is_private: false,
                        name: fresh.clone(),
                        ty: ty.clone(),
                        value: value.clone(),
                        deprecation: None,
                        unstable: None,
                        lint_overrides: Vec::new(),
                    });
                }
                hoisted = true;
                Some(fresh)
            } else {
                None
            };
            self.regions.push(Region {
                name: name.clone(),
                start: end(&s.span),
                end: block_end,
                target,
            });
        }
        if hoisted {
            let marks = self.marks;
            b.stmts.retain(|s| !is_block_const(s, marks));
        }
    }
}

fn is_block_const(s: &Stmt, marks: &FxHashSet<SpanKey>) -> bool {
    matches!(&s.kind, StmtKind::Let { pattern, .. }
        if matches!(pattern.kind, PatternKind::Binding(_))
            && marks.contains(&SpanKey::from_span(&pattern.span)))
}

struct Rename<'a> {
    regions: &'a [Region],
    /// The names a `let` binds, which are declarations, not uses.
    binding_sites: FxHashSet<usize>,
}

impl Rename<'_> {
    /// The innermost declaration of `name` that reaches `offset`.
    fn target(&self, name: &str, offset: usize) -> Option<String> {
        self.regions
            .iter()
            .filter(|r| r.name == name && r.start <= offset && offset < r.end)
            .max_by_key(|r| r.start)
            .and_then(|r| r.target.clone())
    }

    fn rename(&self, name: &mut String, offset: usize) {
        if let Some(t) = self.target(name, offset) {
            *name = t;
        }
    }
}

impl MutVisitor for Rename<'_> {
    fn span(&mut self, _: &mut Span) {}

    fn stmt(&mut self, s: &mut Stmt) {
        if let StmtKind::Let { pattern, .. } = &s.kind {
            if matches!(pattern.kind, PatternKind::Binding(_)) {
                self.binding_sites.insert(pattern.span.offset);
            }
        }
    }

    fn expr(&mut self, e: &mut Expr) {
        let offset = e.span.offset;
        match &mut e.kind {
            ExprKind::Identifier(name) => self.rename(name, offset),
            ExprKind::Path {
                segments,
                generic_args: None,
            } if segments.len() == 1 => self.rename(&mut segments[0], offset),
            _ => {}
        }
    }

    fn pattern(&mut self, p: &mut Pattern) {
        let offset = p.span.offset;
        match &mut p.kind {
            // A constant-class name in a refutable pattern is the constant.
            PatternKind::Binding(name)
                if is_const_class(name) && !self.binding_sites.contains(&offset) =>
            {
                self.rename(name, offset)
            }
            PatternKind::RangePattern { start, end, .. } => {
                for bound in [start, end].into_iter().flatten() {
                    if let RangeBound::Path { segments, span } = bound {
                        if segments.len() == 1 {
                            let at = span.offset;
                            self.rename(&mut segments[0], at);
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// The name a block's constant was written with: `MOD` for the hoisted
/// `MOD__BLOCK1`, for a diagnostic to show. Any other name is returned as is.
pub fn source_name(name: &str) -> &str {
    match name.rfind("__BLOCK") {
        Some(at)
            if at > 0
                && name.len() > at + 7
                && name[at + 7..].bytes().all(|b| b.is_ascii_digit()) =>
        {
            &name[..at]
        }
        _ => name,
    }
}

/// Declare every block's `const` at module scope, just before the item it
/// was in, and point its uses at it. A no-op for a program without one.
pub fn hoist_block_consts(program: &mut Program) {
    if program.block_const_spans.is_empty() {
        return;
    }
    let marks = program.block_const_spans.clone();
    let mut taken: FxHashSet<String> = program
        .items
        .iter()
        .filter_map(|i| match i {
            Item::ConstDecl(c) => Some(c.name.clone()),
            _ => None,
        })
        .collect();
    let items = std::mem::take(&mut program.items);
    for mut item in items {
        let mut found = Found {
            marks: &marks,
            taken: &mut taken,
            regions: Vec::new(),
            decls: Vec::new(),
        };
        walk_item_mut_with(&mut item, &mut found);
        let (regions, decls) = (found.regions, found.decls);
        if !decls.is_empty() {
            let mut rename = Rename {
                regions: &regions,
                binding_sites: FxHashSet::default(),
            };
            walk_item_mut_with(&mut item, &mut rename);
            for mut c in decls {
                walk_expr_mut_with(&mut c.value, &mut rename);
                program.items.push(Item::ConstDecl(c));
            }
        }
        program.items.push(item);
    }
}
